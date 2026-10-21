use super::*;

const P: Params = Params::DEFAULT;

fn machine(cpus: usize) -> JobSched {
    JobSched::new(cpus, P, Nanos::ZERO)
}

fn cpu(c: u32) -> CpuId {
    CpuId(c)
}

/// Run `cpu` from `*now` for `ns`, scheduling first.
fn run(s: &mut JobSched, c: CpuId, now: &mut Nanos, ns: u64) -> Option<ThreadId> {
    let t = s.schedule(c, *now);
    *now = now.after(ns);
    s.charge(c, *now);
    t
}

fn group_lag(s: &JobSched, c: CpuId, job: JobId) -> Lag {
    s.cpus[c.0 as usize].groups.lag(job)
}

fn thread_lag(s: &JobSched, c: CpuId, t: ThreadId) -> Lag {
    let job = s.job_of(t);
    let i = s.group_index(c, job).unwrap();
    s.cpus[c.0 as usize].members[i].threads.lag(t)
}

#[test]
fn a_jobs_group_weights_sum_to_its_weight() {
    let mut s = machine(3);
    let j = s.create_job(3 * WEIGHT_UNIT);
    let other = s.create_job(WEIGHT_UNIT);
    for c in [0, 0, 1, 2] {
        let t = s.spawn(j);
        s.wake(t, cpu(c), Nanos::ZERO);
    }
    let o = s.spawn(other);
    s.wake(o, cpu(2), Nanos::ZERO);
    for c in 0..3 {
        s.schedule(cpu(c), Nanos(1));
    }
    let w: Vec<u64> = (0..3).map(|c| s.group_weight_on(cpu(c), j).unwrap()).collect();
    assert_eq!(w, [3 * WEIGHT_UNIT / 2, 3 * WEIGHT_UNIT / 4, 3 * WEIGHT_UNIT / 4]);
    assert_eq!(w.iter().sum::<u64>(), 3 * WEIGHT_UNIT);
    s.check();
}

#[test]
fn a_helper_buys_no_entitlement() {
    let mut s = machine(1);
    let one = s.create_job(WEIGHT_UNIT);
    let many = s.create_job(WEIGHT_UNIT);
    let lone = s.spawn(one);
    s.wake(lone, cpu(0), Nanos::ZERO);
    for _ in 0..4 {
        let t = s.spawn(many);
        s.wake(t, cpu(0), Nanos::ZERO);
    }
    let mut now = Nanos::ZERO;
    for _ in 0..2000 {
        let t = s.schedule(cpu(0), now).unwrap();
        let ns = s.next_decision(cpu(0)).unwrap().since(now);
        now = now.after(ns);
        s.charge(cpu(0), now);
        let _ = t;
    }
    let (got, _) = s.service(lone);
    // Within the two jobs' request sizes of half the CPU.
    assert!((got as i64 - (now.0 / 2) as i64).abs() <= 2 * P.slice as i64, "{got} of {}", now.0);
}

#[test]
fn a_threads_lag_follows_it_across_migration() {
    let mut s = machine(2);
    let j = s.create_job(WEIGHT_UNIT);
    let rival = s.create_job(WEIGHT_UNIT);
    let (a, b, c) = (s.spawn(j), s.spawn(j), s.spawn(j));
    let r = s.spawn(rival);
    s.wake(a, cpu(0), Nanos::ZERO);
    s.wake(b, cpu(0), Nanos::ZERO);
    s.wake(c, cpu(1), Nanos::ZERO);
    s.wake(r, cpu(1), Nanos::ZERO);
    let mut now = Nanos::ZERO;
    // `a` runs 3 ms on CPU 0 ahead of its sibling: it owes the sibling.
    assert_eq!(run(&mut s, cpu(0), &mut now, 3 * MS), Some(a));
    s.block(cpu(0), now);
    s.wake(a, cpu(0), now);
    s.schedule(cpu(0), now);
    let before = thread_lag(&s, cpu(0), a);
    assert!(before < Lag::ZERO);
    s.migrate(a, cpu(1), now);
    let after = thread_lag(&s, cpu(1), a);
    assert!((after.0 - before.0).abs() <= 2 * WEIGHT_UNIT as i128, "{before:?} became {after:?}");
    s.check();
}

#[test]
fn a_dissolved_groups_lag_goes_with_its_last_thread() {
    let mut s = machine(2);
    let j = s.create_job(WEIGHT_UNIT);
    let rival = s.create_job(WEIGHT_UNIT);
    let other = s.create_job(WEIGHT_UNIT);
    let t = s.spawn(j);
    let r = s.spawn(rival);
    let o = s.spawn(other);
    s.wake(t, cpu(0), Nanos::ZERO);
    s.wake(r, cpu(0), Nanos::ZERO);
    s.wake(o, cpu(1), Nanos::ZERO);
    let mut now = Nanos::ZERO;
    assert_eq!(run(&mut s, cpu(0), &mut now, 3 * MS), Some(t));
    s.schedule(cpu(0), now);
    let debt = group_lag(&s, cpu(0), j);
    assert!(debt < Lag::ZERO);
    s.charge(cpu(1), now);
    s.migrate(t, cpu(1), now);
    let carried = group_lag(&s, cpu(1), j);
    assert!((carried.0 - debt.0).abs() <= 2 * WEIGHT_UNIT as i128, "{debt:?} became {carried:?}");
}

#[test]
fn credit_kept_across_a_sleep_is_bounded() {
    let mut s = machine(1);
    let j = s.create_job(WEIGHT_UNIT);
    let rival = s.create_job(WEIGHT_UNIT);
    let (a, b) = (s.spawn(j), s.spawn(j));
    let r = s.spawn(rival);
    s.wake(r, cpu(0), Nanos::ZERO);
    s.wake(a, cpu(0), Nanos::ZERO);
    s.wake(b, cpu(0), Nanos::ZERO);
    let mut now = Nanos::ZERO;
    // One 50 ms charge with no decision in between: whoever ran leaves the
    // others far behind.
    let first = run(&mut s, cpu(0), &mut now, 50 * MS).unwrap();
    let bound = Lag::from_ns(P.lag_bound as i64).0;
    while s.state(a) != State::Blocked || s.state(b) != State::Blocked {
        let t = s.schedule(cpu(0), now).unwrap();
        if t == r {
            now = now.after(MS);
            s.charge(cpu(0), now);
        } else {
            s.block(cpu(0), now);
            let kept = s.thread(t).left.unwrap().lag;
            assert!(kept.0.abs() <= bound, "{t:?} kept {kept:?}");
        }
    }
    for t in [a, b] {
        assert!(s.thread(t).group_lag.0.abs() <= bound);
    }
    let _ = first;
}

#[test]
fn a_latency_request_changes_order_not_share() {
    let mut s = machine(1);
    let quick = s.create_job(WEIGHT_UNIT);
    let bulk = s.create_job(WEIGHT_UNIT);
    let q = s.spawn(quick);
    let b = s.spawn(bulk);
    s.wake(q, cpu(0), Nanos::ZERO);
    s.wake(b, cpu(0), Nanos::ZERO);
    s.set_slice(q, P.min_slice, Nanos::ZERO);
    let mut now = Nanos::ZERO;
    let mut longest_wait = 0;
    let mut waiting_since = Nanos::ZERO;
    for _ in 0..4000 {
        let t = s.schedule(cpu(0), now).unwrap();
        if t == q {
            longest_wait = longest_wait.max(now.since(waiting_since));
        }
        now = s.next_decision(cpu(0)).unwrap();
        s.charge(cpu(0), now);
        if t == q {
            waiting_since = now;
        }
    }
    let (got_q, _) = s.service(q);
    let (got_b, _) = s.service(b);
    assert!((got_q as i64 - got_b as i64).abs() <= 2 * P.slice as i64);
    // Ahead by at most its own request (EEVDF's `lag > −r`), which the bulk
    // job's service repays at half the rate it accrued: a wait of at most two
    // short requests, not one of the bulk job's.
    assert!(longest_wait <= 2 * P.min_slice, "waited {longest_wait}");
    assert_eq!(s.cpus[0].groups.weight(quick), s.cpus[0].groups.weight(bulk));
}

#[test]
fn an_ordinary_wake_preempts_only_after_the_minimum_run() {
    let mut s = machine(1);
    let a = s.create_job(WEIGHT_UNIT);
    let b = s.create_job(WEIGHT_UNIT);
    let h = s.spawn(a);
    let w = s.spawn(b);
    s.set_slice(w, MS, Nanos::ZERO);
    s.wake(h, cpu(0), Nanos::ZERO);
    let mut now = Nanos::ZERO;
    s.schedule(cpu(0), now);
    let dispatched = now;
    now = now.after(P.min_slice / 2);
    s.charge(cpu(0), now);
    // The waker joins at zero lag with a shorter request than the hog's: due
    // first, but the hog keeps its minimum run.
    assert_eq!(s.wake(w, cpu(0), now), Resched::At(dispatched.after(P.min_slice)));
    assert_eq!(s.next_decision(cpu(0)), Some(dispatched.after(P.min_slice)));
    assert_eq!(s.schedule(cpu(0), dispatched.after(P.min_slice)), Some(w));
}

fn rt_machine(cpus: usize) -> (JobSched, JobId, ThreadId) {
    let mut s = machine(cpus);
    let j = s.create_job(WEIGHT_UNIT);
    let worker = s.spawn(j);
    s.grant_rt(worker, Nanos::ZERO).unwrap();
    (s, j, worker)
}

#[test]
fn exhaustion_demotes_until_the_boundary_wherever_the_worker_goes() {
    let (mut s, _, w) = rt_machine(2);
    let o = s.create_job(WEIGHT_UNIT);
    let ord = s.spawn(o);
    s.wake(ord, cpu(0), Nanos::ZERO);
    s.wake(w, cpu(0), Nanos::ZERO);
    assert_eq!(s.schedule(cpu(0), Nanos::ZERO), Some(w));
    assert_eq!(s.next_decision(cpu(0)), Some(Nanos(P.rt_budget)));
    let now = Nanos(P.rt_budget);
    s.schedule(cpu(0), now);
    assert_eq!(s.state(w), State::Queued { cpu: cpu(0), band: Band::Spare });
    assert_eq!(s.current(cpu(0)), Some((ord, Band::Fair)), "ordinary work runs ahead of spare work");
    // CPU 1's allowance is untouched, but the worker is demoted until CPU 0's
    // boundary: hopping collects no second allowance.
    s.migrate(w, cpu(1), now);
    assert_eq!(s.state(w), State::Queued { cpu: cpu(1), band: Band::Spare });
    assert_eq!(s.next_decision(cpu(1)).map(|d| d.min(Nanos(P.rt_interval))), Some(Nanos(P.rt_interval)));
    s.schedule(cpu(1), Nanos(P.rt_interval));
    assert_eq!(s.state(w), State::Queued { cpu: cpu(1), band: Band::Rt });
    s.check();
}

#[test]
fn neither_a_new_grant_nor_a_wake_resets_demotion_or_the_runaway_count() {
    let (mut s, _, w) = rt_machine(1);
    s.wake(w, cpu(0), Nanos::ZERO);
    s.schedule(cpu(0), Nanos::ZERO);
    let now = Nanos(P.rt_budget);
    s.schedule(cpu(0), now);
    assert_eq!(s.state(w), State::Queued { cpu: cpu(0), band: Band::Spare });
    s.revoke_rt(w, now).unwrap();
    assert_eq!(s.state(w), State::Queued { cpu: cpu(0), band: Band::Fair });
    s.grant_rt(w, now).unwrap();
    assert_eq!(s.state(w), State::Queued { cpu: cpu(0), band: Band::Spare });
    assert_eq!(s.thread(w).demoted_until, Nanos(P.rt_interval));
    assert_eq!(s.thread(w).unblocked_rt, P.rt_budget);
    let remaining = s.allowance(cpu(0)).remaining();
    s.schedule(cpu(0), now);
    s.block(cpu(0), now);
    s.wake(w, cpu(0), now);
    assert_eq!(s.allowance(cpu(0)).remaining(), remaining);
    assert_eq!(s.thread(w).unblocked_rt, 0, "blocking is the one reset");
}

/// J2: spare time is charged to the thread alone — not to the allowance
/// and not to its realtime service — and spare work yields at once to an
/// ordinary wake.
#[test]
fn spare_time_is_charged_to_the_thread_alone_and_yields_to_ordinary_work() {
    let (mut s, _, w) = rt_machine(1);
    let o = s.create_job(WEIGHT_UNIT);
    let ord = s.spawn(o);
    s.wake(w, cpu(0), Nanos::ZERO);
    s.schedule(cpu(0), Nanos::ZERO);
    let mut now = Nanos(P.rt_budget);
    s.schedule(cpu(0), now);
    assert_eq!(s.current(cpu(0)), Some((w, Band::Spare)), "nothing else wants the CPU");
    let (service, remaining) = (s.service(w), s.allowance(cpu(0)).remaining());
    now = now.after(MS);
    s.charge(cpu(0), now);
    assert_eq!(s.spare_service(w), MS);
    assert_eq!((s.service(w), s.allowance(cpu(0)).remaining()), (service, remaining));
    assert_eq!(s.wake(ord, cpu(0), now), Resched::Now);
    assert_eq!(s.schedule(cpu(0), now), Some(ord));
    s.check();
}

#[test]
fn an_owners_strikes_survive_new_workers_and_revoke_them_all() {
    let (mut s, j, w) = rt_machine(2);
    let second = s.spawn(j);
    s.grant_rt(second, Nanos::ZERO).unwrap();
    s.wake(w, cpu(0), Nanos::ZERO);
    s.wake(second, cpu(1), Nanos::ZERO);
    let mut now = Nanos::ZERO;
    // Spin across intervals until one runaway strike lands, then replace the
    // worker by a fresh grant to a new thread: the strike stays with the owner.
    while s.rt_owner(j).strikes == 0 {
        s.schedule(cpu(0), now);
        s.schedule(cpu(1), now);
        now = now.after(MS / 2);
        s.charge(cpu(0), now);
        s.charge(cpu(1), now);
    }
    let strikes = s.rt_owner(j).strikes;
    s.schedule(cpu(0), now);
    if s.current(cpu(0)).is_some_and(|r| r.0 == w) {
        s.block(cpu(0), now);
    }
    s.revoke_rt(w, now).unwrap();
    let fresh = s.spawn(j);
    s.grant_rt(fresh, now).unwrap();
    assert!(s.rt_owner(j).strikes >= strikes);
    s.wake(fresh, cpu(0), now);
    while !s.rt_owner(j).revoked {
        s.schedule(cpu(0), now);
        s.schedule(cpu(1), now);
        now = now.after(MS / 2);
        s.charge(cpu(0), now);
        s.charge(cpu(1), now);
    }
    assert_eq!(s.grant_rt(fresh, now), Err(Refused::OwnerRevoked));
    assert!(!s.thread(second).granted && !s.thread(fresh).granted);
    s.check();
}

#[test]
fn a_reused_thread_slot_inherits_no_grant() {
    let (mut s, j, w) = rt_machine(1);
    s.exit(w);
    assert_eq!(s.grant_rt(w, Nanos::ZERO), Err(Refused::Gone));
    let reused = s.spawn(j);
    assert_eq!(reused.index, w.index);
    assert_ne!(reused, w);
    assert!(!s.thread(reused).granted);
    s.wake(reused, cpu(0), Nanos::ZERO);
    assert_eq!(s.state(reused), State::Queued { cpu: cpu(0), band: Band::Fair });
}

#[test]
fn a_job_owner_lives_exactly_as_long_as_its_references() {
    let mut s = machine(1);
    let j = s.create_job(WEIGHT_UNIT);
    let t = s.spawn(j);
    s.release_job(j);
    assert!(s.job_alive(j));
    s.wake(t, cpu(0), Nanos::ZERO);
    s.schedule(cpu(0), Nanos::ZERO);
    s.block(cpu(0), Nanos(1));
    s.exit(t);
    assert!(!s.job_alive(j));
    let fresh = s.create_job(WEIGHT_UNIT);
    assert_ne!(fresh, j, "a reused slot is a different owner");
    assert_eq!(s.rt_owner(fresh), RtOwner::default());
}

/// Random operations, every invariant checked after each: each runnable
/// thread in exactly one queue, counts matching queues.
#[test]
fn every_runnable_thread_is_in_exactly_one_queue() {
    let mut seed = 11u64;
    let mut next = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    let mut s = machine(3);
    let jobs: Vec<JobId> = (0..3).map(|_| s.create_job(WEIGHT_UNIT)).collect();
    let mut threads: Vec<ThreadId> = Vec::new();
    let mut now = Nanos::ZERO;
    for _ in 0..20_000 {
        now = now.after(next(300_000));
        let c = cpu(next(3) as u32);
        match next(9) {
            0 if threads.len() < 12 => threads.push(s.spawn(jobs[next(3) as usize])),
            1 => {
                let blocked: Vec<_> = threads.iter().copied().filter(|&t| s.state(t) == State::Blocked).collect();
                if let Some(&t) = blocked.get(next(blocked.len().max(1) as u64) as usize) {
                    s.wake(t, c, now);
                }
            }
            2 if s.current(c).is_some() => {
                s.block(c, now);
            }
            3 => {
                let movable: Vec<_> = threads
                    .iter()
                    .copied()
                    .filter(|&t| matches!(s.state(t), State::Queued { .. }))
                    .filter(|&t| (0..3).all(|k| s.current(cpu(k)).map(|r| r.0) != Some(t)))
                    .collect();
                if let Some(&t) = movable.get(next(movable.len().max(1) as u64) as usize) {
                    s.migrate(t, c, now);
                }
            }
            4 if !threads.is_empty() => {
                let t = threads[next(threads.len() as u64) as usize];
                let _ = s.grant_rt(t, now);
            }
            5 if !threads.is_empty() => {
                let t = threads[next(threads.len() as u64) as usize];
                let _ = s.revoke_rt(t, now);
            }
            6 => {
                let blocked: Vec<_> = threads.iter().copied().filter(|&t| s.state(t) == State::Blocked).collect();
                if let Some(&t) = blocked.first() {
                    if next(4) == 0 {
                        s.exit(t);
                        threads.retain(|&x| x != t);
                    }
                }
            }
            _ => {
                s.schedule(c, now);
            }
        }
        s.check();
    }
}

/// An allowance exhausted by a charge that another CPU's move made is a
/// decision now, not a nanosecond later.
#[test]
fn an_allowance_exhausted_by_another_cpus_move_is_a_decision_now() {
    let (mut s, _, w) = rt_machine(2);
    let o = s.create_job(WEIGHT_UNIT);
    let ord = s.spawn(o);
    s.wake(w, cpu(0), Nanos::ZERO);
    s.wake(ord, cpu(0), Nanos::ZERO);
    assert_eq!(s.schedule(cpu(0), Nanos::ZERO), Some(w));
    let exhausted = Nanos(P.rt_budget);
    s.migrate(ord, cpu(1), exhausted);
    assert!(s.allowance(cpu(0)).exhausted());
    assert_eq!(s.next_decision(cpu(0)), Some(exhausted));
}

#[test]
fn revoking_an_exited_worker_is_refused_not_a_panic() {
    let (mut s, _, w) = rt_machine(1);
    s.exit(w);
    assert_eq!(s.revoke_rt(w, Nanos::ZERO), Err(Refused::Gone));
}
