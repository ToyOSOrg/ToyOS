//! The interpreter proper: a definition block's term list run at load, and a
//! control method's run when invoked, by one walk over the bytes (§5.4.2:
//! "the interpretation of the definition block during the definition block
//! loading is similar to the interpretation of the control method").
//!
//! Terms are interpreted as they are read, never parsed ahead: a name in an
//! argument position is resolved when it is reached, so a method invocation
//! takes as many arguments as the method it names declares, and the body of
//! an If not taken is skipped by its PkgLength unread.
//!
//! Every evaluation is bounded: in steps, in nesting (terms, invocations and
//! field accesses together, each a frame of this walk), in the size of any
//! object, and in time asked to sleep. A bound reached is a refusal.
//!
//! The bytes of a string or buffer that an operator makes, at a size its
//! table chooses, are made after its operands are evaluated, and are let go
//! or held against the meter before its target is: an operand, a target and
//! a field's access each run this walk again, and what one holds across
//! that, every level of a nest holds at once.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::cmp::Ordering;

use crate::field::{flags, BufField, Field, Kind, Region};
use crate::name::{text, Path, Seg};
use crate::namespace::{Namespace, NodeId};
use crate::object::{
    bounded, decimal, fit, hex2, joined, slot_of, to_buf, to_int, to_str, Body, Bytes, Elems, Kept, Meter, Method, Mutex,
    Object, Ref, Slot, Unresolved, Width,
};
use crate::stream::{starts_name, Cursor};
use crate::{Error, Host, Usage, MAX_DEPTH, MAX_NESTING, MAX_STEPS, MAX_WAIT_US, REVISION, WINDOWS, WORK_PER_STEP};

pub(crate) struct Machine<'a> {
    pub(crate) ns: &'a mut Namespace,
    pub(crate) host: &'a mut dyn Host,
    pub(crate) w: Width,
    meter: Rc<Meter>,
    steps: u64,
    depth: u32,
    waited_us: u64,
    /// Every Mutex acquire not yet released, in order.
    held: Vec<Kept<Mutex>>,
    /// The SyncLevel of each held Mutex and running Serialized method, in
    /// order; the last is the current level (§19.6.88).
    levels: Vec<u8>,
    global: u32,
}

pub(crate) struct Frame {
    locals: [Slot; 8],
    args: Vec<Slot>,
    scope: NodeId,
    table: Bytes,
    held: usize,
}

pub(crate) enum Flow {
    Next,
    Break,
    Continue,
    Return(Object),
}

enum Target {
    None,
    Debug,
    Local(usize),
    Arg(usize),
    Node(NodeId),
    Ref(Ref),
}

fn slot(o: Object) -> Slot {
    Rc::new(RefCell::new(o))
}

impl Frame {
    pub(crate) fn new(scope: NodeId, args: Vec<Object>, table: Bytes, held: usize) -> Frame {
        Frame {
            locals: core::array::from_fn(|_| slot(Object::Uninit)),
            args: args.into_iter().map(slot).collect(),
            scope,
            table,
            held,
        }
    }
}

fn type_name(code: u64) -> &'static [u8] {
    match code {
        4 => b"[Package]",
        5 => b"[Field]",
        6 => b"[Device]",
        7 => b"[Event]",
        8 => b"[Control Method]",
        9 => b"[Mutex]",
        10 => b"[Operation Region]",
        11 => b"[Power Resource]",
        12 => b"[Processor]",
        13 => b"[Thermal Zone]",
        14 => b"[Buffer Field]",
        16 => b"[Debug Object]",
        _ => b"[Uninitialized Object]",
    }
}

/// One step more, refused past the bound.
fn tick(steps: &mut u64) -> Result<(), Error> {
    *steps += 1;
    if *steps > MAX_STEPS { Err(Error::Bound("more steps than one evaluation may take")) } else { Ok(()) }
}

impl<'a> Machine<'a> {
    pub(crate) fn new(ns: &'a mut Namespace, host: &'a mut dyn Host, w: Width, meter: Rc<Meter>) -> Self {
        Machine { ns, host, w, meter, steps: 0, depth: 0, waited_us: 0, held: Vec::new(), levels: Vec::new(), global: 0 }
    }

    /// Steps for `bytes` of work done in one: a step a [`WORK_PER_STEP`].
    pub(crate) fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.steps = self.steps.saturating_add((bytes / WORK_PER_STEP) as u64);
        self.step()
    }

    /// Bytes made, charged for and held against the meter.
    pub(crate) fn bytes(&mut self, v: Vec<u8>) -> Result<Bytes, Error> {
        self.charge(v.len())?;
        self.meter.bytes(v)
    }

    pub(crate) fn new_str(&mut self, v: Vec<u8>) -> Result<Object, Error> {
        Ok(Object::Str(self.bytes(v)?))
    }

    pub(crate) fn new_buf(&mut self, v: Vec<u8>) -> Result<Object, Error> {
        Ok(Object::Buf(self.bytes(v)?))
    }

    /// A package of `count` elements, held against the meter before any is
    /// made and charged for: what fills it is as large as its table says.
    fn new_pkg(&mut self, count: usize) -> Result<Elems, Error> {
        let elems = self.meter.package(count)?;
        self.charge(count * crate::object::ELEMENT)?;
        Ok(elems)
    }

    /// A copy of an object for a store (§19.3.5.8): data is duplicated,
    /// anything else is the same object again. Bounded in nesting, which a
    /// table can grow without limit by storing a package into itself.
    pub(crate) fn copy(&mut self, o: &Object) -> Result<Object, Error> {
        self.copy_in(o, 0)
    }

    fn copy_in(&mut self, o: &Object, depth: usize) -> Result<Object, Error> {
        if depth > MAX_NESTING {
            return Err(Error::Bound("a package nests deeper than this interpreter copies"));
        }
        match o {
            Object::Str(s) => {
                let v = s.borrow().clone();
                self.new_str(v)
            }
            Object::Buf(b) => {
                let v = b.borrow().clone();
                self.new_buf(v)
            }
            Object::Pkg(p) => Ok(Object::Pkg(self.copy_pkg(p, depth)?)),
            other => Ok(other.clone()),
        }
    }

    fn copy_pkg(&mut self, p: &Elems, depth: usize) -> Result<Elems, Error> {
        let count = p.borrow().len();
        let out = self.new_pkg(count)?;
        for i in 0..count {
            let e = p.borrow()[i].clone();
            out.set(i, self.copy_in(&e, depth + 1)?)?;
        }
        Ok(out)
    }

    /// A copy for a package element or a named object, which a reference that
    /// lives only in a LocalX or ArgX never enters (the module header of
    /// `object`).
    fn lasting(&mut self, v: &Object) -> Result<Object, Error> {
        if v.frame_bound() {
            return Err(Error::Type("a reference to a package element, LocalX or ArgX stored where it would outlive its method"));
        }
        self.copy(v)
    }

    pub(crate) fn step(&mut self) -> Result<(), Error> {
        tick(&mut self.steps)
    }

    /// A NameString read (§20.2.2), charged for its bytes: a table writes as
    /// many parent prefixes and segments as it likes.
    fn name(&mut self, c: &mut Cursor<'_>) -> Result<Path, Error> {
        let at = c.at;
        let p = c.name()?;
        self.charge(c.at - at)?;
        Ok(p)
    }

    /// The object a path names from `scope` (§5.3), if it names one.
    fn find(&mut self, scope: NodeId, p: &Path) -> Result<Option<NodeId>, Error> {
        let steps = &mut self.steps;
        self.ns.resolve(scope, p, &mut || tick(steps))
    }

    /// The absolute path of a node, and of `child` below it when given.
    pub(crate) fn path_of(&mut self, id: NodeId, child: Option<Seg>) -> Result<String, Error> {
        let steps = &mut self.steps;
        self.ns.path_of(id, child, &mut || tick(steps))
    }

    /// DerefOf of a String names an object by ASL text (§19.6.30), read whole.
    fn path_of_text(&mut self, s: &Bytes) -> Result<Path, Error> {
        self.charge(s.borrow().len())?;
        Path::text(&s.borrow()).ok_or(Error::Rule("DerefOf of a String that is not a name (§19.6.30)"))
    }

    pub(crate) fn enter(&mut self) -> Result<(), Error> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::Bound("terms, invocations and field accesses nest deeper than this interpreter goes"));
        }
        self.depth += 1;
        Ok(())
    }

    pub(crate) fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Ends an evaluation: whatever it still holds is let go, and holding
    /// anything at its end is itself a refusal (§19.6.88: "the top-level
    /// control method cannot exit while still holding ownership of a Mutex").
    /// What it took goes to `took`, whichever way it ends.
    pub(crate) fn finish<T>(mut self, r: Result<T, Error>, took: &mut Usage) -> Result<T, Error> {
        (took.steps, took.waited_us) = (self.steps, self.waited_us);
        let held = !self.held.is_empty();
        for m in self.held.drain(..) {
            m.held.set(0);
        }
        self.levels.clear();
        let mut released = Ok(());
        if self.global > 0 {
            self.global = 0;
            released = self.host.global_release().map_err(|d| Error::Host(d.0));
        }
        let r = r?;
        released?;
        if held {
            return Err(Error::Rule("an evaluation ends holding a Mutex (§19.6.88)"));
        }
        Ok(r)
    }

    /// Holds the Global Lock once more, taking it from the host where this
    /// evaluation does not hold it yet: `Ok(false)` is a take `within` a
    /// bound that timed out, charged as a wait, and nothing held.
    pub(crate) fn take_global(&mut self, within: Option<u16>) -> Result<bool, Error> {
        if self.global == 0 {
            match within {
                None => self.host.global_take().map_err(|d| Error::Host(d.0))?,
                Some(ms) if !self.host.global_take_within(ms).map_err(|d| Error::Host(d.0))? => {
                    self.wait(u64::from(ms) * 1000)?;
                    return Ok(false);
                }
                Some(_) => {}
            }
        }
        self.global += 1;
        Ok(true)
    }

    pub(crate) fn drop_global(&mut self) -> Result<(), Error> {
        self.global = self.global.checked_sub(1).expect("the Global Lock is given back only by who took it");
        if self.global == 0 {
            self.host.global_release().map_err(|d| Error::Host(d.0))?;
        }
        Ok(())
    }

    fn wait(&mut self, us: u64) -> Result<(), Error> {
        self.waited_us = self.waited_us.saturating_add(us);
        if self.waited_us > MAX_WAIT_US {
            return Err(Error::Bound("more time asleep than one evaluation may spend"));
        }
        Ok(())
    }

    fn define(&mut self, f: &Frame, p: &Path, o: Object) -> Result<NodeId, Error> {
        let steps = &mut self.steps;
        self.ns.create(f.scope, p, o, &mut || tick(steps))
    }

    fn resolve(&mut self, f: &Frame, p: &Path) -> Result<NodeId, Error> {
        self.find(f.scope, p)?.ok_or_else(|| Error::NotFound(text(p)))
    }

    fn node_object(&self, id: NodeId) -> Result<Object, Error> {
        self.ns.object(id).cloned().ok_or_else(|| Error::NotFound(String::from("an object its method's exit destroyed")))
    }

    /// The integer a named child of `scope` evaluates to, if it exists.
    pub(crate) fn named_int(&mut self, scope: NodeId, seg: Seg) -> Result<Option<u64>, Error> {
        let Some(id) = self.ns.child(scope, seg) else { return Ok(None) };
        let v = match self.node_object(id)? {
            Object::Method(m) if m.args == 0 => self.invoke(id, m, Vec::new())?,
            _ => self.node_value(id)?,
        };
        to_int(&v, self.w).map(Some)
    }

    pub(crate) fn evaluate(&mut self, id: NodeId, args: Vec<Object>) -> Result<Object, Error> {
        match self.node_object(id)? {
            Object::Method(m) => {
                if args.len() != usize::from(m.args) {
                    return Err(Error::Rule("an evaluation passes a method another number of arguments than it declares"));
                }
                self.invoke(id, m, args)
            }
            _ if !args.is_empty() => Err(Error::Rule("an evaluation passes arguments to an object that is not a method")),
            _ => self.node_value(id),
        }
    }

    /// What a named object evaluates to in an argument position: a field is
    /// read, data is itself, anything else is a reference to it (§19.6.101:
    /// "Named References to non-Data Objects ... are instead returned ... as
    /// references").
    fn node_value(&mut self, id: NodeId) -> Result<Object, Error> {
        Ok(match self.node_object(id)? {
            Object::Field(f) => self.read_field(&f)?,
            Object::BufField(b) => self.read_buf_field(&b)?,
            o @ (Object::Int(_) | Object::Str(_) | Object::Buf(_) | Object::Pkg(_) | Object::Ref(_)) => o,
            _ => Object::Ref(Ref::Node(id)),
        })
    }

    /// The object a reference refers to (§19.6.30).
    fn deref(&mut self, r: &Ref) -> Result<Object, Error> {
        match r {
            Ref::Node(id) => self.node_value(*id),
            Ref::Slot(s) => Ok(slot_of(s)?.borrow().clone()),
            Ref::Elem(p, i) => {
                let e = p.borrow().get(*i).cloned().ok_or(Error::Rule("an Index reference past its package's end"))?;
                match e {
                    Object::Uninit => Err(Error::Rule("DerefOf an uninitialized package element (§19.6.30)")),
                    Object::Lazy(l) => self.lazy(&l),
                    e => Ok(e),
                }
            }
            Ref::BufField(b) => self.read_buf_field(b),
        }
    }

    /// A package element a name gave (§19.6.101), if `scope` resolves the
    /// name: data is resolved to its value, anything else is a reference.
    fn element(&mut self, scope: NodeId, p: &Path) -> Result<Option<Object>, Error> {
        let Some(id) = self.find(scope, p)? else { return Ok(None) };
        Ok(Some(match self.node_value(id)? {
            d @ (Object::Int(_) | Object::Str(_) | Object::Buf(_) | Object::Pkg(_)) => self.copy(&d)?,
            o => o,
        }))
    }

    fn lazy(&mut self, l: &Unresolved) -> Result<Object, Error> {
        self.element(l.scope, &l.path)?.ok_or_else(|| Error::NotFound(text(&l.path)))
    }

    pub(crate) fn resolve_lazy(&mut self, o: Object) -> Result<Object, Error> {
        match o {
            Object::Lazy(l) => self.lazy(&l),
            o => Ok(o),
        }
    }

    // ---- invocation -------------------------------------------------------

    pub(crate) fn invoke(&mut self, node: NodeId, m: Kept<Method>, args: Vec<Object>) -> Result<Object, Error> {
        self.enter()?;
        let r = self.invoke_in(node, &m, args);
        self.leave();
        r
    }

    fn invoke_in(&mut self, node: NodeId, m: &Method, args: Vec<Object>) -> Result<Object, Error> {
        let (table, start, end) = match &m.body {
            Body::Osi => return self.osi(args),
            Body::Aml { table, start, end } => (table.clone(), *start, *end),
        };
        if m.serialized {
            if self.levels.last().is_some_and(|&l| m.sync < l) {
                return Err(Error::Rule("a Serialized method's SyncLevel is below the current level (§19.6.88)"));
            }
            self.levels.push(m.sync);
        }
        let made = self.ns.mark();
        let mut f = Frame::new(node, args, table.clone(), self.held.len());
        let bytes = table.borrow();
        let mut c = Cursor::new(&bytes, start, end);
        let flow = self.term_list(&mut f, &mut c);
        self.ns.unwind(made);
        if m.serialized {
            self.levels.pop();
        }
        let flow = flow?;
        if self.held.len() > f.held {
            return Err(Error::Rule("a method exits holding a Mutex it acquired (§19.6.88)"));
        }
        match flow {
            Flow::Next => Ok(Object::Uninit),
            Flow::Return(v) => Ok(v),
            Flow::Break | Flow::Continue => Err(Error::Rule("a Break or Continue outside any While (§19.6.8, §19.6.16)")),
        }
    }

    /// `\_OSI` (§5.7.2), answered as the owner ruled: yes to every Windows
    /// version string Microsoft publishes, no to anything else.
    fn osi(&mut self, args: Vec<Object>) -> Result<Object, Error> {
        let Some(Object::Str(s)) = args.first() else {
            return Err(Error::Type("_OSI's argument is not a String (§5.7.2)"));
        };
        Ok(Object::Int(self.w.bool(WINDOWS.iter().any(|w| w.as_bytes() == s.borrow().as_slice()))))
    }

    // ---- term lists -------------------------------------------------------

    pub(crate) fn term_list(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        while !c.done() {
            match self.term(f, c)? {
                Flow::Next => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Next)
    }

    fn term(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        self.step()?;
        self.enter()?;
        let r = self.term_in(f, c);
        self.leave();
        r
    }

    fn term_in(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        let op = c.peek()?;
        if starts_name(op) {
            // MethodInvocation (§20.2.5): a name alone in a term list.
            let p = self.name(c)?;
            let id = self.resolve(f, &p)?;
            let Object::Method(m) = self.node_object(id)? else {
                return Err(c.malformed("a name in a term list names no method (§20.2.5)"));
            };
            let args = self.call_args(f, c, &m)?;
            self.invoke(id, m, args)?;
            return Ok(Flow::Next);
        }
        match op {
            0x06 => self.def_alias(f, c),
            0x08 => self.def_name(f, c),
            0x10 => self.def_scope(f, c),
            0x14 => self.def_method(f, c),
            0x15 => self.def_external(c),
            0x8A | 0x8B | 0x8C | 0x8D | 0x8F => self.def_create_field(f, c),
            0x86 => self.notify(f, c),
            0xA0 => self.if_else(f, c),
            0xA1 => Err(c.malformed("an Else that follows no If (§20.2.5.3)")),
            0xA2 => self.while_loop(f, c),
            0xA3 | 0xCC => {
                // Noop, and BreakPoint, which outside a debugger "is equivalent to Noop" (§19.6.9).
                c.byte()?;
                Ok(Flow::Next)
            }
            0xA4 => {
                c.byte()?;
                let v = self.arg(f, c)?;
                Ok(Flow::Return(self.copy(&v)?))
            }
            0xA5 => {
                c.byte()?;
                Ok(Flow::Break)
            }
            0x9F => {
                c.byte()?;
                Ok(Flow::Continue)
            }
            0x5B => match c.peek2()? {
                0x01 => self.def_mutex(f, c),
                0x02 => self.def_event(f, c),
                0x13 => self.def_create_field(f, c),
                0x80 => self.def_region(f, c),
                0x81 | 0x86 | 0x87 => self.def_field(f, c),
                0x82..=0x85 => self.def_scoped(f, c),
                0x88 => Err(Error::Unsupported("DataTableRegion")),
                0x20 => Err(Error::Unsupported("Load")),
                0x21 | 0x22 => self.delay(f, c),
                0x24 | 0x26 | 0x27 => self.sync_statement(f, c),
                0x32 => self.fatal(f, c),
                0x12 | 0x1F | 0x23 | 0x25 | 0x28 | 0x29 | 0x33 => self.expr(f, c).map(|_| Flow::Next),
                _ => Err(c.malformed("not a term (§20.2.5)")),
            },
            0x11..=0x13 | 0x70..=0x85 | 0x87..=0x89 | 0x8E | 0x90..=0x99 | 0x9C..=0x9E => {
                self.expr(f, c).map(|_| Flow::Next)
            }
            _ => Err(c.malformed("not a term (§20.2.5)")),
        }
    }

    fn call_args(&mut self, f: &mut Frame, c: &mut Cursor<'_>, m: &Method) -> Result<Vec<Object>, Error> {
        (0..m.args).map(|_| self.arg(f, c)).collect()
    }

    fn sub<'c>(c: &Cursor<'c>, end: usize) -> Cursor<'c> {
        Cursor::new(c.bytes, c.at, end)
    }

    fn if_else(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let end = c.pkg_end()?;
        let mut body = Self::sub(c, end);
        let taken = self.int_arg(f, &mut body)? != 0;
        let flow = if taken { self.term_list(f, &mut body)? } else { Flow::Next };
        c.at = end;
        if !c.done() && c.peek()? == 0xA1 {
            c.byte()?;
            let end = c.pkg_end()?;
            if !taken {
                let mut alt = Self::sub(c, end);
                let flow = self.term_list(f, &mut alt)?;
                c.at = end;
                return Ok(flow);
            }
            c.at = end;
        }
        Ok(flow)
    }

    fn while_loop(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let end = c.pkg_end()?;
        let start = c.at;
        loop {
            self.step()?;
            let mut body = Cursor::new(c.bytes, start, end);
            if self.int_arg(f, &mut body)? == 0 {
                break;
            }
            match self.term_list(f, &mut body)? {
                Flow::Next | Flow::Continue => {}
                Flow::Break => break,
                Flow::Return(v) => return Ok(Flow::Return(v)),
            }
        }
        c.at = end;
        Ok(Flow::Next)
    }

    // ---- named objects (§20.2.5.1, §20.2.5.2) ------------------------------

    fn def_alias(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let source = self.name(c)?;
        let alias = self.name(c)?;
        let target = self.resolve(f, &source)?;
        let steps = &mut self.steps;
        self.ns.alias(f.scope, &alias, target, &mut || tick(steps))?;
        Ok(Flow::Next)
    }

    fn def_name(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let p = self.name(c)?;
        let v = self.data_object(f, c)?;
        self.define(f, &p, v)?;
        Ok(Flow::Next)
    }

    fn def_scope(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let end = c.pkg_end()?;
        let mut body = Self::sub(c, end);
        let p = self.name(&mut body)?;
        let id = self.resolve(f, &p)?;
        // §19.6.120: a Scope's location is a predefined scope, a Device, a
        // Processor, a Thermal Zone or a Power Resource.
        if !matches!(
            self.node_object(id)?,
            Object::Scope | Object::Device | Object::Processor | Object::ThermalZone | Object::PowerResource
        ) {
            return Err(Error::Type("a Scope names an object that opens no scope (§19.6.120)"));
        }
        let flow = self.within(f, id, &mut body)?;
        c.at = end;
        Ok(flow)
    }

    fn within(&mut self, f: &mut Frame, scope: NodeId, body: &mut Cursor<'_>) -> Result<Flow, Error> {
        let outer = core::mem::replace(&mut f.scope, scope);
        let flow = self.term_list(f, body);
        f.scope = outer;
        flow
    }

    /// Device, Processor, PowerResource and ThermalZone (§20.2.5.2): a named
    /// object whose term list runs in its own scope.
    fn def_scoped(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let op = c.byte()?;
        let end = c.pkg_end()?;
        let mut body = Self::sub(c, end);
        let p = self.name(&mut body)?;
        let o = match op {
            0x82 => Object::Device,
            0x85 => Object::ThermalZone,
            0x83 => {
                // DefProcessor := ProcessorOp PkgLength NameString ProcID
                // PblkAddr PblkLen TermList (ACPI 6.3A §20.2.5.2), parsed by
                // the owner's ruling to accept what real firmware ships.
                body.byte()?;
                body.dword()?;
                body.byte()?;
                Object::Processor
            }
            _ => {
                // SystemLevel and ResourceOrder, which only OSPM's power
                // resource management reads.
                body.byte()?;
                body.word()?;
                Object::PowerResource
            }
        };
        let id = self.define(f, &p, o)?;
        let flow = self.within(f, id, &mut body)?;
        c.at = end;
        Ok(flow)
    }

    fn def_method(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let end = c.pkg_end()?;
        let mut head = Self::sub(c, end);
        let p = self.name(&mut head)?;
        let flags = head.byte()?;
        let m = Method {
            body: Body::Aml { table: f.table.clone(), start: head.at, end },
            args: flags & 0x07,
            serialized: flags & 0x08 != 0,
            sync: flags >> 4,
        };
        let m = self.meter.hold(m)?;
        self.define(f, &p, Object::Method(m))?;
        c.at = end;
        Ok(Flow::Next)
    }

    /// External (§20.2.5.2) tells a disassembler what another table defines,
    /// and defines nothing.
    fn def_external(&mut self, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        self.name(c)?;
        c.byte()?;
        c.byte()?;
        Ok(Flow::Next)
    }

    fn def_mutex(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        c.byte()?;
        let p = self.name(c)?;
        // SyncFlags: the SyncLevel in bits 0-3, the rest reserved (§20.2.5.2).
        let sync = c.byte()? & 0x0F;
        let m = self.meter.hold(Mutex { sync, held: Cell::new(0), global: false })?;
        self.define(f, &p, Object::Mutex(m))?;
        Ok(Flow::Next)
    }

    fn def_event(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        c.byte()?;
        let p = self.name(c)?;
        let e = self.meter.hold(Cell::new(0))?;
        self.define(f, &p, Object::Event(e))?;
        Ok(Flow::Next)
    }

    fn def_region(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        c.byte()?;
        let p = self.name(c)?;
        let space = c.byte()?;
        let base = self.int_arg(f, c)?;
        let len = self.int_arg(f, c)?;
        let r = self.meter.hold(Region { space, base, len, scope: f.scope })?;
        self.define(f, &p, Object::Region(r))?;
        Ok(Flow::Next)
    }

    fn field_of(&mut self, f: &Frame, c: &mut Cursor<'_>) -> Result<Kept<Field>, Error> {
        let p = self.name(c)?;
        let id = self.resolve(f, &p)?;
        match self.node_object(id)? {
            Object::Field(x) => Ok(x),
            _ => Err(Error::Type("an IndexField's or BankField's register is not a field unit (§19.6.63, §19.6.7)")),
        }
    }

    fn region_of(&mut self, f: &Frame, c: &mut Cursor<'_>) -> Result<Kept<Region>, Error> {
        let p = self.name(c)?;
        let id = self.resolve(f, &p)?;
        match self.node_object(id)? {
            Object::Region(r) => Ok(r),
            _ => Err(Error::Type("a field's RegionName is not an operation region (§19.6.47)")),
        }
    }

    /// Field, IndexField and BankField (§20.2.5.2): a FieldList of named
    /// units laid out bit after bit.
    fn def_field(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let op = c.byte()?;
        let end = c.pkg_end()?;
        let mut l = Self::sub(c, end);
        let kind = match op {
            0x81 => Kind::Region(self.region_of(f, &mut l)?),
            0x86 => {
                let index = self.field_of(f, &mut l)?;
                let data = self.field_of(f, &mut l)?;
                Kind::Index { index, data }
            }
            _ => {
                let region = self.region_of(f, &mut l)?;
                let bank = self.field_of(f, &mut l)?;
                let value = self.int_arg(f, &mut l)?;
                Kind::Bank { region, bank, value }
            }
        };
        let (mut access, lock, update) = flags(l.byte()?);
        let mut bit = 0u64;
        while !l.done() {
            self.step()?;
            match l.peek()? {
                0x00 => {
                    l.byte()?;
                    bit = bit.checked_add(l.pkg_value()?.1 as u64).ok_or(l.malformed("a FieldList overflows"))?;
                }
                0x01 | 0x03 => {
                    let ext = l.byte()? == 0x03;
                    access = flags(l.byte()?).0;
                    l.byte()?;
                    if ext {
                        l.byte()?;
                    }
                }
                0x02 => {
                    // ConnectField: the connection of a GeneralPurposeIO or
                    // GenericSerialBus field, which this interpreter does not
                    // access.
                    l.byte()?;
                    if l.peek()? == 0x11 {
                        self.data_object(f, &mut l)?;
                    } else {
                        self.name(&mut l)?;
                    }
                }
                _ => {
                    let seg = l.seg()?;
                    let len = l.pkg_value()?.1 as u64;
                    if len == 0 {
                        return Err(l.malformed("a field unit of zero bits"));
                    }
                    let to = bit.checked_add(len).filter(|&e| e <= 1 << 62).ok_or(l.malformed("a FieldList overflows"))?;
                    let unit = self.meter.hold(Field { kind: kind.clone(), bit, len, access, lock, update })?;
                    self.define(f, &Path { root: false, up: 0, segs: vec![seg] }, Object::Field(unit))?;
                    bit = to;
                }
            }
        }
        c.at = end;
        Ok(Flow::Next)
    }

    /// CreateBitField, CreateByteField, CreateWordField, CreateDWordField,
    /// CreateQWordField and CreateField (§19.6.18-23): a field over a buffer,
    /// which must hold it whole.
    fn def_create_field(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        let op = c.byte()?;
        let op = if op == 0x5B { c.byte()? } else { op };
        // SourceBuff is evaluated as a buffer (§19.6.18-23): a Buffer is the
        // one the field reaches into, anything else converts to a new one.
        let data = match self.arg(f, c)? {
            Object::Buf(b) => b,
            o => {
                let v = to_buf(&o, self.w)?;
                self.bytes(v)?
            }
        };
        let index = self.int_arg(f, c)?;
        let (bit, len) = match op {
            0x8D => (Some(index), 1),
            0x8C => (index.checked_mul(8), 8),
            0x8B => (index.checked_mul(8), 16),
            0x8A => (index.checked_mul(8), 32),
            0x8F => (index.checked_mul(8), 64),
            _ => {
                let n = self.int_arg(f, c)?;
                if n == 0 {
                    return Err(Error::Rule("CreateField of zero bits (§19.6.21)"));
                }
                (Some(index), n)
            }
        };
        let p = self.name(c)?;
        let size = (data.borrow().len() as u64).saturating_mul(8);
        let bit = bit.filter(|b| b.checked_add(len).is_some_and(|e| e <= size));
        let bit = bit.ok_or(Error::Rule("a buffer field reaches past its buffer (§19.6.18-23)"))?;
        let field = self.meter.hold(BufField { data, bit, len })?;
        self.define(f, &p, Object::BufField(field))?;
        Ok(Flow::Next)
    }

    // ---- statements (§20.2.5.3) -------------------------------------------

    fn notify(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let t = self.super_name(f, c)?;
        let v = self.int_arg(f, c)?;
        let id = self.node_of(f, &t)?;
        // §19.6.94: a device, processor, or thermal zone.
        if !matches!(self.node_object(id)?, Object::Device | Object::Processor | Object::ThermalZone) {
            return Err(Error::Type("Notify of an object that is not a device, processor or thermal zone (§19.6.94)"));
        }
        let path = self.path_of(id, None)?;
        self.host.notify(&path, v);
        Ok(Flow::Next)
    }

    fn delay(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let sleep = c.byte()? == 0x22;
        let n = self.int_arg(f, c)?;
        if sleep {
            self.wait(n.saturating_mul(1000))?;
            self.host.sleep(n);
        } else {
            // UsecTime := TermArg => ByteData (§20.2.5.3).
            if n > 0xFF {
                return Err(Error::Rule("a Stall longer than the 255 µs its ByteData holds (§20.2.5.3)"));
            }
            self.wait(n)?;
            self.host.stall(n);
        }
        Ok(Flow::Next)
    }

    fn fatal(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        c.byte()?;
        let kind = c.byte()?;
        let code = c.dword()?;
        let arg = self.int_arg(f, c)?;
        Err(Error::Fatal { kind, code, arg })
    }

    /// Signal, Reset and Release.
    fn sync_statement(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Flow, Error> {
        c.byte()?;
        let op = c.byte()?;
        let t = self.super_name(f, c)?;
        let id = self.node_of(f, &t)?;
        match (op, self.node_object(id)?) {
            (0x24, Object::Event(e)) => e.set(e.get().saturating_add(1)),
            (0x26, Object::Event(e)) => e.set(0),
            (0x27, Object::Mutex(m)) => self.release(&m)?,
            _ => return Err(Error::Type("Signal, Reset or Release of an object of the wrong type (§19.6)")),
        }
        Ok(Flow::Next)
    }

    /// `false` where `\_GL`'s firmware side held it past `within`
    /// milliseconds, and nothing was acquired.
    fn acquire(&mut self, m: &Kept<Mutex>, within: Option<u16>) -> Result<bool, Error> {
        // §19.6.88: an Acquire's SyncLevel is equal to or above the current one.
        if m.held.get() == 0 && self.levels.last().is_some_and(|&l| m.sync < l) {
            return Err(Error::Rule("Acquire of a Mutex below the current SyncLevel (§19.6.88)"));
        }
        if m.global && m.held.get() == 0 && !self.take_global(within)? {
            return Ok(false);
        }
        m.held.set(m.held.get() + 1);
        self.held.push(m.clone());
        self.levels.push(m.sync);
        Ok(true)
    }

    fn release(&mut self, m: &Kept<Mutex>) -> Result<(), Error> {
        if m.held.get() == 0 {
            return Err(Error::Rule("Release of a Mutex not held (§19.6.115)"));
        }
        // §19.6.88: a Release's SyncLevel is the current one.
        if self.levels.last() != Some(&m.sync) {
            return Err(Error::Rule("Release of a Mutex whose SyncLevel is not the current one (§19.6.88)"));
        }
        let at = self.held.iter().rposition(|h| Rc::ptr_eq(h, m)).ok_or(Error::Rule("Release of a Mutex not held"))?;
        self.held.remove(at);
        self.levels.pop();
        m.held.set(m.held.get() - 1);
        if m.global && m.held.get() == 0 {
            self.drop_global()?;
        }
        Ok(())
    }

    // ---- arguments --------------------------------------------------------

    pub(crate) fn int_arg(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<u64, Error> {
        let o = self.arg(f, c)?;
        to_int(&o, self.w)
    }

    /// A TermArg (§20.2.5).
    pub(crate) fn arg(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        self.step()?;
        self.enter()?;
        let r = self.arg_in(f, c);
        self.leave();
        r
    }

    fn arg_in(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let op = c.peek()?;
        if starts_name(op) {
            return self.named(f, c);
        }
        match op {
            0x60..=0x67 => {
                c.byte()?;
                Ok(f.locals[usize::from(op - 0x60)].borrow().clone())
            }
            0x68..=0x6E => self.read_arg(f, c, usize::from(op - 0x68)),
            0x00 | 0x01 | 0xFF | 0x0A..=0x0E => self.data_object(f, c),
            0x5B if c.peek2()? == 0x30 => self.data_object(f, c),
            0x5B if c.peek2()? == 0x31 => Err(Error::Type("the Debug object is write-only (§19.6.26)")),
            _ => self.expr(f, c),
        }
    }

    /// A name in an argument position: a method it names is invoked.
    fn named(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let p = self.name(c)?;
        let id = self.resolve(f, &p)?;
        match self.node_object(id)? {
            Object::Method(m) => {
                let args = self.call_args(f, c, &m)?;
                self.invoke(id, m, args)
            }
            _ => self.node_value(id),
        }
    }

    fn read_arg(&mut self, f: &mut Frame, c: &mut Cursor<'_>, i: usize) -> Result<Object, Error> {
        c.byte()?;
        let v = f.args.get(i).ok_or(c.malformed("an ArgX past the method's argument count"))?.borrow().clone();
        // Table 19.9: reading an ArgX that holds a reference reads its target.
        match v {
            Object::Ref(r) => self.deref(&r),
            v => Ok(v),
        }
    }

    /// A DataObject (§20.2.3), as Name defines and a package holds.
    fn data_object(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let ones = self.w.ones();
        Ok(match c.byte()? {
            0x00 => Object::Int(0),
            0x01 => Object::Int(1),
            0xFF => Object::Int(ones),
            0x0A => Object::Int(u64::from(c.byte()?)),
            0x0B => Object::Int(u64::from(c.word()?)),
            0x0C => Object::Int(u64::from(c.dword()?) & ones),
            0x0E => Object::Int(c.qword()? & ones),
            0x0D => {
                let mut s = Vec::new();
                loop {
                    match c.byte()? {
                        0 => break,
                        // AsciiChar is 0x01-0x7F (§20.2.3); a byte above it
                        // still ends nothing, and is kept.
                        b => s.push(b),
                    }
                    bounded(s.len())?;
                }
                self.new_str(s)?
            }
            0x11 => {
                let end = c.pkg_end()?;
                let mut b = Self::sub(c, end);
                let size = self.int_arg(f, &mut b)?;
                let size = usize::try_from(size).map_err(|_| Error::Bound("a buffer larger than this interpreter holds"))?;
                bounded(size)?;
                let mut v = c.bytes.get(b.at..end).ok_or(c.malformed("a Buffer runs past the table"))?.to_vec();
                // §19.6.10: the larger of BufferSize and the initializer's length.
                if v.len() < size {
                    v.resize(size, 0);
                }
                c.at = end;
                self.new_buf(v)?
            }
            op @ (0x12 | 0x13) => {
                let end = c.pkg_end()?;
                let mut p = Self::sub(c, end);
                let count = if op == 0x12 { u64::from(p.byte()?) } else { self.int_arg(f, &mut p)? };
                let count = usize::try_from(count).map_err(|_| Error::Bound("a package larger than this interpreter holds"))?;
                let elems = self.new_pkg(count)?;
                let mut read = 0;
                while !p.done() {
                    self.step()?;
                    if read == count {
                        return Err(p.malformed("a package holds more elements than its NumElements (§19.6.101)"));
                    }
                    let e = if starts_name(p.peek()?) {
                        let path = self.name(&mut p)?;
                        match self.element(f.scope, &path)? {
                            Some(e) => e,
                            None => Object::Lazy(self.meter.unresolved(path, f.scope)?),
                        }
                    } else {
                        self.enter()?;
                        let e = self.data_object(f, &mut p);
                        self.leave();
                        e?
                    };
                    elems.set(read, e)?;
                    read += 1;
                }
                c.at = end;
                Object::Pkg(elems)
            }
            0x5B if c.byte()? == 0x30 => Object::Int(REVISION),
            _ => return Err(Error::Malformed { at: c.at.saturating_sub(1), why: "not a DataObject (§20.2.3)" }),
        })
    }

    // ---- targets (§20.2.2) ------------------------------------------------

    fn super_name(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Target, Error> {
        match self.super_name_or(f, c)? {
            Ok(t) => Ok(t),
            Err(p) => Err(Error::NotFound(text(&p))),
        }
    }

    /// A SuperName, or the name it is when that does not resolve, which
    /// CondRefOf answers rather than refuses (§19.6.14).
    fn super_name_or(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Result<Target, Path>, Error> {
        let op = c.peek()?;
        if starts_name(op) {
            let p = self.name(c)?;
            return Ok(self.find(f.scope, &p)?.map(Target::Node).ok_or(p));
        }
        Ok(Ok(match op {
            0x60..=0x67 => {
                c.byte()?;
                Target::Local(usize::from(op - 0x60))
            }
            0x68..=0x6E => {
                c.byte()?;
                let i = usize::from(op - 0x68);
                if i >= f.args.len() {
                    return Err(c.malformed("an ArgX past the method's argument count"));
                }
                Target::Arg(i)
            }
            0x5B if c.peek2()? == 0x31 => {
                c.byte()?;
                c.byte()?;
                Target::Debug
            }
            0x83 => {
                // DerefOf as a SuperName names what its operand refers to.
                c.byte()?;
                match self.arg(f, c)? {
                    Object::Ref(r) => Target::Ref(r),
                    Object::Str(s) => {
                        let p = self.path_of_text(&s)?;
                        Target::Node(self.resolve(f, &p)?)
                    }
                    _ => return Err(Error::Type("DerefOf of an object that is not a reference or a name (§19.6.30)")),
                }
            }
            0x71 | 0x88 => match self.expr(f, c)? {
                Object::Ref(r) => Target::Ref(r),
                _ => return Err(Error::Type("a SuperName that is not a reference")),
            },
            _ => return Err(c.malformed("not a SuperName (§20.2.2)")),
        }))
    }

    fn target(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Target, Error> {
        if c.peek()? == 0x00 {
            c.byte()?;
            return Ok(Target::None);
        }
        self.super_name(f, c)
    }

    /// The node a target names, directly or by a reference it holds.
    fn node_of(&self, f: &Frame, t: &Target) -> Result<NodeId, Error> {
        let held = match t {
            Target::Node(id) => return Ok(*id),
            Target::Ref(Ref::Node(id)) => return Ok(*id),
            Target::Local(i) => f.locals[*i].borrow().clone(),
            Target::Arg(i) => f.args[*i].borrow().clone(),
            _ => Object::Uninit,
        };
        match held {
            Object::Ref(Ref::Node(id)) => Ok(id),
            _ => Err(Error::Type("an operand that names no object in the namespace")),
        }
    }

    /// The object a target is, unread and unconverted, a reference followed
    /// once (§19.6.96: "the object type of the base object").
    fn base(&mut self, f: &Frame, t: &Target) -> Result<Object, Error> {
        let o = match t {
            Target::None | Target::Debug => return Err(Error::Type("the Debug object is write-only (§19.6.26)")),
            Target::Local(i) => f.locals[*i].borrow().clone(),
            Target::Arg(i) => f.args[*i].borrow().clone(),
            Target::Node(id) => self.node_object(*id)?,
            Target::Ref(r) => Object::Ref(r.clone()),
        };
        self.followed(o)
    }

    /// An object, a reference followed once to what it refers to.
    fn followed(&mut self, o: Object) -> Result<Object, Error> {
        Ok(match o {
            Object::Ref(Ref::Node(id)) => self.node_object(id)?,
            Object::Ref(Ref::Slot(s)) => slot_of(&s)?.borrow().clone(),
            Object::Ref(Ref::Elem(p, i)) => {
                let e = p.borrow().get(i).cloned().ok_or(Error::Rule("an Index reference past its package's end"))?;
                self.resolve_lazy(e)?
            }
            Object::Ref(Ref::BufField(b)) => Object::BufField(b),
            o => o,
        })
    }

    /// The value a target holds, read (for Increment and Decrement).
    fn read_target(&mut self, f: &Frame, t: &Target) -> Result<Object, Error> {
        match t {
            Target::Local(i) => Ok(f.locals[*i].borrow().clone()),
            Target::Arg(i) => {
                let v = f.args[*i].borrow().clone();
                match v {
                    Object::Ref(r) => self.deref(&r),
                    v => Ok(v),
                }
            }
            Target::Node(id) => self.node_value(*id),
            Target::Ref(r) => self.deref(r),
            Target::None | Target::Debug => Err(Error::Type("the Debug object is write-only (§19.6.26)")),
        }
    }

    // ---- store and copy (§19.3.5.8) ---------------------------------------

    /// Store, and every operator's Target: no conversion into a LocalX or an
    /// ArgX, which an ArgX holding a reference stores through; conversion to
    /// the type a named object already has.
    fn store(&mut self, f: &mut Frame, t: Target, v: Object) -> Result<(), Error> {
        match t {
            Target::None | Target::Debug => Ok(()),
            Target::Local(i) => {
                let v = self.copy(&v)?;
                *f.locals[i].borrow_mut() = v;
                Ok(())
            }
            Target::Arg(i) => {
                let held = f.args[i].borrow().clone();
                match held {
                    Object::Ref(r) => self.store_ref(&r, v),
                    _ => {
                        let v = self.copy(&v)?;
                        *f.args[i].borrow_mut() = v;
                        Ok(())
                    }
                }
            }
            Target::Node(id) => self.store_node(id, v),
            Target::Ref(r) => self.store_ref(&r, v),
        }
    }

    fn store_ref(&mut self, r: &Ref, v: Object) -> Result<(), Error> {
        match r {
            Ref::Node(id) => self.store_node(*id, v),
            Ref::Slot(s) => {
                let s = slot_of(s)?;
                let v = self.copy(&v)?;
                *s.borrow_mut() = v;
                Ok(())
            }
            Ref::Elem(p, i) => {
                let v = self.lasting(&v)?;
                p.set(*i, v)
            }
            Ref::BufField(b) => self.write_buf_field(b, v),
        }
    }

    fn store_node(&mut self, id: NodeId, v: Object) -> Result<(), Error> {
        let w = self.w;
        match self.node_object(id)? {
            Object::Int(_) => self.ns.set(id, Object::Int(to_int(&v, w)?)),
            Object::Str(s) => {
                let n = to_str(&v, w)?;
                self.charge(n.len())?;
                s.replace(n)
            }
            Object::Buf(b) => {
                // Table 19.7: a buffer that exists keeps its size.
                let n = to_buf(&v, w)?;
                let len = b.borrow().len();
                self.charge(n.len().max(len))?;
                b.replace(fit(n, len))
            }
            Object::Pkg(p) => match &v {
                Object::Pkg(src) => {
                    p.swap(&*self.copy_pkg(src, 0)?);
                    Ok(())
                }
                _ => Err(Error::Type("a store to a package of an object that is not one (Table 19.6)")),
            },
            Object::Field(x) => self.write_field(&x, v),
            Object::BufField(b) => self.write_buf_field(&b, v),
            Object::Ref(_) => {
                let v = self.lasting(&v)?;
                self.ns.set(id, v)
            }
            _ => Err(Error::Type("a store to an object that is not data (Table 19.6)")),
        }
    }

    /// CopyObject, and the explicit conversions' Target (§19.3.5.5): no
    /// conversion; a named object takes the copy's type, a field keeps its
    /// own and takes only an Integer or a Buffer (Table 19.8).
    fn copy_to(&mut self, f: &mut Frame, t: Target, v: Object) -> Result<(), Error> {
        match t {
            Target::Node(id) => self.copy_node(id, v),
            Target::Ref(Ref::Node(id)) => self.copy_node(id, v),
            Target::Ref(Ref::BufField(b)) => match v {
                Object::Int(_) | Object::Buf(_) => self.write_buf_field(&b, v),
                _ => Err(Error::Type("CopyObject to a field of an object that is not an Integer or Buffer (Table 19.8)")),
            },
            Target::Arg(i) => {
                let held = f.args[i].borrow().clone();
                match held {
                    Object::Ref(Ref::Node(id)) => self.copy_node(id, v),
                    Object::Ref(r) => self.store_ref(&r, v),
                    _ => {
                        let v = self.copy(&v)?;
                        *f.args[i].borrow_mut() = v;
                        Ok(())
                    }
                }
            }
            t => self.store(f, t, v),
        }
    }

    fn copy_node(&mut self, id: NodeId, v: Object) -> Result<(), Error> {
        match (self.node_object(id)?, &v) {
            (Object::Field(x), Object::Int(_) | Object::Buf(_)) => self.write_field(&x, v),
            (Object::BufField(b), Object::Int(_) | Object::Buf(_)) => self.write_buf_field(&b, v),
            (Object::Field(_) | Object::BufField(_), _) => {
                Err(Error::Type("CopyObject to a field of an object that is not an Integer or Buffer (Table 19.8)"))
            }
            (Object::Int(_) | Object::Str(_) | Object::Buf(_) | Object::Pkg(_) | Object::Ref(_), _) => {
                let v = self.lasting(&v)?;
                self.ns.set(id, v)
            }
            _ => Err(Error::Type("CopyObject's destination is not a data object (§19.6.17)")),
        }
    }

    // ---- expressions (§20.2.5.4) ------------------------------------------

    /// An ExpressionOpcode, dispatched to one function a family so that the
    /// frames a nested expression stacks stay small.
    fn expr(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let at = c.at;
        match c.byte()? {
            0x11..=0x13 => {
                c.at = at;
                self.data_object(f, c)
            }
            0x70 => self.store_op(f, c),
            0x9D => self.copy_op(f, c),
            op @ (0x72 | 0x74 | 0x77 | 0x79 | 0x7A | 0x7B | 0x7C | 0x7D | 0x7E | 0x7F | 0x85) => self.binary(f, c, op),
            0x78 => self.divide(f, c),
            op @ 0x80..=0x82 => self.unary(f, c, op),
            op @ (0x75 | 0x76) => self.step_op(f, c, op),
            op @ 0x90..=0x92 => self.logic(f, c, op),
            op @ 0x93..=0x95 => self.compare_op(f, c, op),
            0x73 => self.concat(f, c),
            0x84 => self.concat_res(f, c),
            op @ (0x96..=0x99 | 0x9C | 0x9E) => self.convert(f, c, op),
            0x71 => {
                let t = self.super_name(f, c)?;
                Ok(Object::Ref(self.ref_of(f, t)?))
            }
            0x83 => self.deref_op(f, c),
            0x88 => self.index_op(f, c),
            0x87 => self.size_of(f, c),
            0x8E => self.object_type(f, c),
            0x89 => self.match_op(f, c),
            0x5B => self.ext(f, c, at),
            _ => Err(Error::Malformed { at, why: "not an expression (§20.2.5.4)" }),
        }
    }

    fn store_op(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let v = self.arg(f, c)?;
        let t = self.super_name(f, c)?;
        self.store(f, t, v.clone())?;
        Ok(v)
    }

    fn copy_op(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let v = self.arg(f, c)?;
        let t = match c.peek()? {
            0x60..=0x6E => self.super_name(f, c)?,
            b if starts_name(b) => self.super_name(f, c)?,
            _ => return Err(c.malformed("CopyObject's destination is not a SimpleName (§20.2.5.4)")),
        };
        self.copy_to(f, t, v.clone())?;
        Ok(v)
    }

    fn binary(&mut self, f: &mut Frame, c: &mut Cursor<'_>, op: u8) -> Result<Object, Error> {
        let w = self.w;
        let a = self.int_arg(f, c)?;
        let b = self.int_arg(f, c)?;
        let t = self.target(f, c)?;
        let wide = |n: u64| n >= u64::from(w.bits);
        let r = match op {
            0x72 => a.wrapping_add(b),
            0x74 => a.wrapping_sub(b),
            0x77 => a.wrapping_mul(b),
            0x79 if wide(b) => 0,
            0x79 => a << b,
            0x7A if wide(b) => 0,
            0x7A => a >> b,
            0x7B => a & b,
            0x7C => !(a & b),
            0x7D => a | b,
            0x7E => !(a | b),
            0x7F => a ^ b,
            _ => a.checked_rem(b).ok_or(Error::Rule("Mod by zero (§19.6.86)"))?,
        } & w.ones();
        self.store(f, t, Object::Int(r))?;
        Ok(Object::Int(r))
    }

    fn divide(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let a = self.int_arg(f, c)?;
        let b = self.int_arg(f, c)?;
        let rem = self.target(f, c)?;
        let quo = self.target(f, c)?;
        let q = a.checked_div(b).ok_or(Error::Rule("Divide by zero (§19.6.32)"))?;
        self.store(f, rem, Object::Int(a % b))?;
        self.store(f, quo, Object::Int(q))?;
        Ok(Object::Int(q))
    }

    fn unary(&mut self, f: &mut Frame, c: &mut Cursor<'_>, op: u8) -> Result<Object, Error> {
        let a = self.int_arg(f, c)?;
        let t = self.target(f, c)?;
        let r = match op {
            0x80 => !a & self.w.ones(),
            // One-based bit positions, 0 for none set (§19.6.48, §19.6.49).
            0x81 => 64 - u64::from(a.leading_zeros()),
            _ if a == 0 => 0,
            _ => u64::from(a.trailing_zeros()) + 1,
        };
        self.store(f, t, Object::Int(r))?;
        Ok(Object::Int(r))
    }

    /// Increment and Decrement (§19.6.61, §19.6.27).
    fn step_op(&mut self, f: &mut Frame, c: &mut Cursor<'_>, op: u8) -> Result<Object, Error> {
        let t = self.super_name(f, c)?;
        let v = self.read_target(f, &t)?;
        let v = to_int(&v, self.w)?;
        let r = if op == 0x75 { v.wrapping_add(1) } else { v.wrapping_sub(1) } & self.w.ones();
        self.store(f, t, Object::Int(r))?;
        Ok(Object::Int(r))
    }

    fn logic(&mut self, f: &mut Frame, c: &mut Cursor<'_>, op: u8) -> Result<Object, Error> {
        let a = self.int_arg(f, c)? != 0;
        let r = match op {
            0x92 => !a,
            0x90 => self.int_arg(f, c)? != 0 && a,
            _ => self.int_arg(f, c)? != 0 || a,
        };
        Ok(Object::Int(self.w.bool(r)))
    }

    fn compare_op(&mut self, f: &mut Frame, c: &mut Cursor<'_>, op: u8) -> Result<Object, Error> {
        let a = self.arg(f, c)?;
        let b = self.arg(f, c)?;
        let o = self.compare(&a, &b)?;
        let want = match op {
            0x93 => Ordering::Equal,
            0x94 => Ordering::Greater,
            _ => Ordering::Less,
        };
        Ok(Object::Int(self.w.bool(o == want)))
    }

    fn deref_op(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        match self.arg(f, c)? {
            Object::Ref(r) => self.deref(&r),
            Object::Str(s) => {
                let id = {
                    let p = self.path_of_text(&s)?;
                    self.resolve(f, &p)?
                };
                self.node_value(id)
            }
            _ => Err(Error::Type("DerefOf of an object that is not a reference or a name (§19.6.30)")),
        }
    }

    fn index_op(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let src = self.arg(f, c)?;
        let i = self.int_arg(f, c)?;
        let t = self.target(f, c)?;
        let r = self.index(src, i)?;
        self.store(f, t, Object::Ref(r.clone()))?;
        Ok(Object::Ref(r))
    }

    fn size_of(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let t = self.super_name(f, c)?;
        let n = match self.base(f, &t)? {
            Object::Buf(b) | Object::Str(b) => b.borrow().len(),
            Object::Pkg(p) => p.borrow().len(),
            _ => return Err(Error::Type("SizeOf of an object that is not a buffer, string or package (§19.6.124)")),
        };
        Ok(Object::Int(n as u64))
    }

    fn object_type(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let t = self.super_name(f, c)?;
        let code = match t {
            Target::Debug => 16,
            t => self.base(f, &t)?.type_code(),
        };
        Ok(Object::Int(code))
    }

    /// The `ExtOpPrefix` expressions.
    fn ext(&mut self, f: &mut Frame, c: &mut Cursor<'_>, at: usize) -> Result<Object, Error> {
        match c.byte()? {
            0x12 => self.cond_ref_of(f, c),
            0x23 => self.acquire_op(f, c),
            0x25 => self.wait_op(f, c),
            0x28 => self.bcd_in(f, c),
            0x29 => self.bcd_out(f, c),
            0x33 => Ok(Object::Int(self.host.timer() & self.w.ones())),
            0x1F => Err(Error::Unsupported("LoadTable")),
            0x20 => Err(Error::Unsupported("Load")),
            _ => Err(Error::Malformed { at, why: "not an expression (§20.2.5.4)" }),
        }
    }

    fn cond_ref_of(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let found = self.super_name_or(f, c)?;
        let t = self.target(f, c)?;
        match found {
            Ok(s) => {
                let r = self.ref_of(f, s)?;
                self.store(f, t, Object::Ref(r))?;
                Ok(Object::Int(self.w.ones()))
            }
            Err(_) => Ok(Object::Int(0)),
        }
    }

    fn acquire_op(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let t = self.super_name(f, c)?;
        // §19.6.2: 0xFFFF waits with no bound.
        let within = Some(c.word()?).filter(|&ms| ms != 0xFFFF);
        let id = self.node_of(f, &t)?;
        let Object::Mutex(m) = self.node_object(id)? else {
            return Err(Error::Type("Acquire of an object that is not a Mutex (§19.6.2)"));
        };
        // One invocation runs at a time, so no Mutex is owned by another
        // invocation: only `\_GL`'s firmware side can time an Acquire out.
        Ok(Object::Int(if self.acquire(&m, within)? { 0 } else { self.w.ones() }))
    }

    fn wait_op(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let t = self.super_name(f, c)?;
        let timeout = self.int_arg(f, c)?;
        let id = self.node_of(f, &t)?;
        let Object::Event(e) = self.node_object(id)? else {
            return Err(Error::Type("Wait on an object that is not an Event (§19.6.147)"));
        };
        if e.get() > 0 {
            e.set(e.get() - 1);
            return Ok(Object::Int(0));
        }
        // Nothing else runs while this invocation waits, so no signal can
        // arrive: the wait times out.
        if timeout >= 0xFFFF {
            return Err(Error::Rule("a Wait without timeout on an Event no other invocation can signal"));
        }
        self.wait(timeout.saturating_mul(1000))?;
        self.host.sleep(timeout);
        Ok(Object::Int(self.w.ones()))
    }

    fn bcd_in(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let v = self.int_arg(f, c)?;
        let t = self.target(f, c)?;
        let mut r = 0u64;
        for i in (0..16).rev() {
            let d = v >> (4 * i) & 0xF;
            if d > 9 {
                return Err(Error::Rule("FromBCD of a digit above 9 (§19.6.54)"));
            }
            r = r * 10 + d;
        }
        let r = r & self.w.ones();
        self.store(f, t, Object::Int(r))?;
        Ok(Object::Int(r))
    }

    fn bcd_out(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let mut v = self.int_arg(f, c)?;
        let t = self.target(f, c)?;
        let mut r = 0u64;
        for i in 0..self.w.bytes() * 2 {
            r |= (v % 10) << (4 * i);
            v /= 10;
        }
        if v != 0 {
            return Err(Error::Rule("ToBCD of a value with more digits than an integer holds (§19.6.135)"));
        }
        self.store(f, t, Object::Int(r))?;
        Ok(Object::Int(r))
    }

    fn ref_of(&self, f: &Frame, t: Target) -> Result<Ref, Error> {
        match t {
            Target::Node(id) => Ok(Ref::Node(id)),
            Target::Local(i) => Ok(Ref::Slot(Rc::downgrade(&f.locals[i]))),
            Target::Arg(i) => Ok(Ref::Slot(Rc::downgrade(&f.args[i]))),
            Target::Ref(r) => Ok(r),
            Target::None | Target::Debug => Err(Error::Type("a reference to the Debug object")),
        }
    }

    /// Index (§19.6.62): a package's nth element, or a buffer's or string's
    /// nth byte as an 8-bit buffer field.
    fn index(&self, src: Object, i: u64) -> Result<Ref, Error> {
        let past = Error::Rule("Index past the end of its source (§19.6.62)");
        let i = usize::try_from(i).map_err(|_| Error::Rule("Index past the end of its source (§19.6.62)"))?;
        match src {
            Object::Buf(b) | Object::Str(b) => {
                if i >= b.borrow().len() {
                    return Err(past);
                }
                Ok(Ref::BufField(self.meter.hold(BufField { data: b, bit: i as u64 * 8, len: 8 })?))
            }
            Object::Pkg(p) => {
                if i >= p.borrow().len() {
                    return Err(past);
                }
                Ok(Ref::Elem(p, i))
            }
            _ => Err(Error::Type("Index of an object that is not a buffer, string or package (§19.6.62)")),
        }
    }

    /// The logical comparisons' order (§19.6.69-72): the first operand's type
    /// is the one the second converts to; strings and buffers compare byte by
    /// byte, a shorter equal prefix the lesser.
    fn compare(&mut self, a: &Object, b: &Object) -> Result<Ordering, Error> {
        let len = |o: &Object| match o {
            Object::Str(x) | Object::Buf(x) => x.borrow().len(),
            _ => 0,
        };
        self.charge(len(a).max(len(b)))?;
        match a {
            Object::Int(x) => Ok((*x & self.w.ones()).cmp(&to_int(b, self.w)?)),
            Object::Str(x) => Ok(x.borrow().as_slice().cmp(to_str(b, self.w)?.as_slice())),
            Object::Buf(x) => Ok(x.borrow().as_slice().cmp(to_buf(b, self.w)?.as_slice())),
            _ => Err(Error::Type("a comparison of an object that is not an integer, string or buffer (§19.6.69)")),
        }
    }

    /// Concatenate (§19.6.12, Table 19.30).
    fn concat(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let a = self.arg(f, c)?;
        let b = self.arg(f, c)?;
        let t = self.target(f, c)?;
        let w = self.w;
        let data = |o: &Object| matches!(o, Object::Int(_) | Object::Str(_) | Object::Buf(_));
        let named = |m: &mut Self, o: &Object| -> Result<Vec<u8>, Error> { Ok(type_name(m.followed(o.clone())?.type_code()).to_vec()) };
        let tail_str = |m: &mut Self, o: &Object| if data(o) { to_str(o, w) } else { named(m, o) };
        let r = match &a {
            Object::Int(x) => {
                let mut v = w.le(*x);
                v.extend(w.le(to_int(&b, w)?));
                self.new_buf(v)?
            }
            Object::Str(x) => {
                let tail = tail_str(self, &b)?;
                let mut v = x.borrow().clone();
                v.extend(tail);
                self.new_str(v)?
            }
            Object::Buf(x) => {
                let tail = if data(&b) {
                    to_buf(&b, w)?
                } else {
                    let mut n = named(self, &b)?;
                    n.push(0);
                    n
                };
                let mut v = x.borrow().clone();
                v.extend(tail);
                self.new_buf(v)?
            }
            other => {
                let mut v = named(self, other)?;
                v.extend(tail_str(self, &b)?);
                self.new_str(v)?
            }
        };
        self.store(f, t, r.clone())?;
        Ok(r)
    }

    /// ConcatenateResTemplate (§19.6.13): both templates' descriptors, then a
    /// new End Tag whose checksum makes the bytes sum to zero (§6.4.2.9).
    fn concat_res(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let a = self.arg(f, c)?;
        let b = self.arg(f, c)?;
        let t = self.target(f, c)?;
        let mut out = Vec::new();
        for o in [&a, &b] {
            let v = to_buf(o, self.w)?;
            match v.len() {
                0 => {}
                1 => return Err(Error::Rule("ConcatenateResTemplate of a one-byte template (§19.6.13)")),
                n if v[n - 2] == 0x79 => out.extend_from_slice(&v[..n - 2]),
                _ => return Err(Error::Rule("ConcatenateResTemplate of a template without an End Tag (§6.4.2.9)")),
            }
        }
        out.push(0x79);
        let sum = out.iter().fold(0u8, |s, &x| s.wrapping_add(x));
        out.push(0u8.wrapping_sub(sum));
        let r = self.new_buf(out)?;
        self.store(f, t, r.clone())?;
        Ok(r)
    }

    /// The explicit conversions (§19.3.5.2) and Mid (§19.6.85).
    fn convert(&mut self, f: &mut Frame, c: &mut Cursor<'_>, op: u8) -> Result<Object, Error> {
        let w = self.w;
        let src = self.arg(f, c)?;
        let r = match op {
            0x96 => {
                let v = to_buf(&src, w)?;
                self.new_buf(v)?
            }
            0x97 => match &src {
                Object::Int(v) => self.new_str(decimal(*v))?,
                Object::Str(s) => {
                    let v = s.borrow().clone();
                    self.new_str(v)?
                }
                Object::Buf(b) => {
                    bounded(b.borrow().len().saturating_mul(4))?;
                    // Each byte written out as digits: the input's bytes are work too.
                    self.charge(b.borrow().len().saturating_mul(4))?;
                    let v = joined(&b.borrow(), b',', |x, out| out.extend(decimal(u64::from(x))));
                    self.new_str(v)?
                }
                _ => return Err(Error::Type("ToDecimalString of an object that is not an integer, string or buffer")),
            },
            // §19.6.138 names no form for a buffer's values; each is written
            // in the two-digit form the Buffer to String rule uses (Table 19.7).
            0x98 => match &src {
                Object::Buf(b) => {
                    bounded(b.borrow().len().saturating_mul(3))?;
                    // Each byte written out as digits: the input's bytes are work too.
                    self.charge(b.borrow().len().saturating_mul(4))?;
                    let v = joined(&b.borrow(), b',', hex2);
                    self.new_str(v)?
                }
                o => {
                    let v = to_str(o, w)?;
                    self.new_str(v)?
                }
            },
            0x99 => Object::Int(match &src {
                Object::Str(s) => {
                    self.charge(s.borrow().len())?;
                    int_of_text(&s.borrow(), w)?
                }
                o => to_int(o, w)?,
            }),
            0x9C => {
                let n = self.int_arg(f, c)?;
                let n = if n == w.ones() { usize::MAX } else { usize::try_from(n).unwrap_or(usize::MAX) };
                let b = to_buf(&src, w)?;
                self.charge(b.len())?;
                self.new_str(b.iter().take(n).take_while(|&&x| x != 0).copied().collect())?
            }
            _ => {
                let i = self.int_arg(f, c)?;
                let n = self.int_arg(f, c)?;
                let r = {
                    let (data, is_str) = match &src {
                        Object::Str(s) => (s.borrow().clone(), true),
                        o => (to_buf(o, w)?, false),
                    };
                    self.charge(data.len())?;
                    let start = usize::try_from(i).unwrap_or(usize::MAX).min(data.len());
                    let end = start.saturating_add(usize::try_from(n).unwrap_or(usize::MAX)).min(data.len());
                    let part = data[start..end].to_vec();
                    if is_str { self.new_str(part)? } else { self.new_buf(part)? }
                };
                let t = self.target(f, c)?;
                self.store(f, t, r.clone())?;
                return Ok(r);
            }
        };
        let t = self.target(f, c)?;
        self.copy_to(f, t, r.clone())?;
        Ok(r)
    }

    /// Match (§19.6.80).
    fn match_op(&mut self, f: &mut Frame, c: &mut Cursor<'_>) -> Result<Object, Error> {
        let Object::Pkg(p) = self.arg(f, c)? else {
            return Err(Error::Type("Match of an object that is not a package (§19.6.80)"));
        };
        let op1 = c.byte()?;
        let m1 = self.arg(f, c)?;
        let op2 = c.byte()?;
        let m2 = self.arg(f, c)?;
        let start = self.int_arg(f, c)?;
        if op1 > 5 || op2 > 5 {
            return Err(c.malformed("a MatchOpcode above 5 (§20.2.5.4)"));
        }
        for m in [&m1, &m2] {
            if !matches!(m, Object::Int(_) | Object::Str(_) | Object::Buf(_)) {
                return Err(Error::Type("a MatchObject that is not an integer, string or buffer (§19.6.80)"));
            }
        }
        let len = p.borrow().len();
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        for i in start..len {
            self.step()?;
            let e = p.borrow().get(i).cloned().unwrap_or(Object::Uninit);
            let e = match e {
                Object::Lazy(l) => self.lazy(&l)?,
                e => e,
            };
            if !matches!(e, Object::Int(_) | Object::Str(_) | Object::Buf(_)) {
                continue;
            }
            if self.matches(&e, op1, &m1)? && self.matches(&e, op2, &m2)? {
                return Ok(Object::Int(i as u64));
            }
        }
        Ok(Object::Int(self.w.ones()))
    }

    /// One Match comparison: the element converted to the MatchObject's
    /// type, an element that does not convert matching nothing; a bound
    /// reached on the way is a refusal.
    fn matches(&mut self, e: &Object, op: u8, m: &Object) -> Result<bool, Error> {
        if op == 0 {
            return Ok(true);
        }
        let converted = match m {
            Object::Int(_) => to_int(e, self.w).map(Object::Int),
            Object::Str(_) => to_str(e, self.w).and_then(|v| self.new_str(v)),
            _ => to_buf(e, self.w).and_then(|v| self.new_buf(v)),
        };
        let e = match converted {
            Ok(e) => e,
            Err(Error::Type(_)) => return Ok(false),
            Err(other) => return Err(other),
        };
        let o = match self.compare(&e, m) {
            Ok(o) => o,
            Err(Error::Type(_)) => return Ok(false),
            Err(other) => return Err(other),
        };
        Ok(match op {
            1 => o == Ordering::Equal,
            2 => o != Ordering::Greater,
            3 => o == Ordering::Less,
            4 => o != Ordering::Less,
            _ => o == Ordering::Greater,
        })
    }
}

/// ToInteger of a String (§19.6.139): decimal, or hexadecimal after `0x`;
/// a value an integer cannot hold is refused, as is anything else.
fn int_of_text(s: &[u8], w: Width) -> Result<u64, Error> {
    let (digits, radix) = match s {
        [b'0', b'x' | b'X', rest @ ..] => (rest, 16),
        _ => (s, 10),
    };
    if digits.is_empty() {
        return Err(Error::Type("ToInteger of a string with no digits (§19.6.139)"));
    }
    let mut v = 0u64;
    for &ch in digits {
        let d = char::from(ch).to_digit(radix).ok_or(Error::Type("ToInteger of a string that is not a number (§19.6.139)"))?;
        v = v
            .checked_mul(u64::from(radix))
            .and_then(|v| v.checked_add(u64::from(d)))
            .filter(|&v| v <= w.ones())
            .ok_or(Error::Rule("ToInteger of a number larger than an integer holds (§19.6.139)"))?;
    }
    Ok(v)
}
