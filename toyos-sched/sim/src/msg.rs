//! Type aliases that pin the core's generics to the simulator's payload.

use toyos_sched::cpu::{CpuHandles, CpuSched, Env, SchedPass};
use toyos_sched::msg::Msg;
use toyos_sched::watch::Watch;

use crate::hw_impl::SimHw;
use crate::payload::{NoRing, SimPayload, SimPreempt, SimWatchList};

pub type SimMsg = Msg<SimPayload>;
pub type SimCpu = CpuSched<SimPayload>;
pub type SimHandles = CpuHandles<SimMsg>;
pub type SimQueue = Watch<SimMsg, NoRing, SimWatchList>;
pub type SimEnv<'e> = Env<'e, SimHw, SimPreempt>;
pub type SimPass<'c, 'e, S> = SchedPass<'c, 'e, SimHw, SimPreempt, S>;
