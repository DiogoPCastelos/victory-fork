//! Where the per-lane form of a branch on a lane mask is not the console's.
//!
//! [`crate::structure`] turns `s_cbranch_execz`/`execnz`/`vccz`/`vccnz` (and SCC of a mask
//! operation) into per-lane branches: each lane goes by its own bit. The console decides once
//! for the whole wave, and every lane follows. Both agree on what a lane's vector code does
//! when the region the branch skips runs under the mask the branch tests: lanes outside it
//! do nothing there either way. They disagree in two cases, which the lane tests show
//! (`emit/far_jump_tests.rs`, whole-wave mask branches):
//!
//! - **Scalar code read after the region.** A scalar the region writes (an SGPR, SCC, M0)
//!   holds, on the console, the wave's value after it; per lane, a lane that did not run the
//!   region keeps the old one. Loop counters read after a lane-mask loop are the common case.
//! - **A branch on a mask other than EXEC** (`vccz`/`vccnz`) over vector code, if the mask
//!   differs between active lanes: on the console the region runs under EXEC in every active
//!   lane when some lane's bit says so; per lane, only that lane runs it. Compilers write such
//!   a branch for conditions they know to be uniform (then the forms agree), so [`inexact`]
//!   does not count it.
//!
//! [`inexact`] finds those branches for the census (`victory translate --list`: "per-lane
//! branches not exact") and the lane tests; the translator does not act on it yet. Taking
//! such a branch wave-wide ([`crate::structure::structure_wide`]) keeps EXEC as a guard as
//! the console does.

use std::collections::{HashMap, HashSet};

use crate::ir::{EXEC, Exit, Program, Reg, Value};
use crate::structure::{Atom, LaneExit, Structured};

/// A per-lane branch whose per-lane form is not exact, by the block it closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inexact {
    pub block: usize,
    pub why: &'static str,
}

pub const SCALAR_AFTER: &str = "a scalar written where only some lanes run is read after it";

pub const INDIRECT: &str = "an indirect jump: the regions are not known";

/// Every per-lane branch ([`Structured::exits`], not wave-wide, not uniform) whose per-lane
/// form is not exact, with the first reason found.
pub fn inexact(p: &Program, st: &Structured) -> Vec<Inexact> {
    let n = p.blocks.len();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, block) in p.blocks.iter().enumerate() {
        match block.exit {
            Exit::Jump(t) => succ[b].push(t),
            Exit::Branch { taken, not_taken } => succ[b].extend([taken, not_taken]),
            Exit::End => {}
            _ => return lane_branches(st).into_iter().map(|block| Inexact { block, why: INDIRECT }).collect(),
        }
    }
    let pdom = post_dominators(&succ);
    let scalar = |r: Reg| match r {
        Reg::Scc => true,
        Reg::S(s) => s != EXEC && s != EXEC + 1,
        Reg::V(_) => false,
    };
    // lane masks (and their joins) are lane data, which the per-lane form follows
    let mask = |v: Value| st.masks.contains_key(&v) || st.highs.contains_key(&v) || st.phi_masks.contains_key(&v);
    // where each value is made, and the value each plain copy (`s_mov_b32`/`_b64` of an SGPR,
    // a phi of one value) is of
    let mut defined = HashMap::<Value, usize>::new();
    let mut copy = HashMap::<Value, Value>::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for phi in &block.phis {
            defined.insert(phi.value, b);
        }
        for i in &block.insts {
            let moves = matches!(i.machine.name(), Some("s_mov_b32" | "s_mov_b64"));
            for (k, &(r, w)) in i.writes.iter().enumerate() {
                defined.insert(w, b);
                if moves && matches!(r, Reg::S(_))
                    && let Some(&(Reg::S(_), from)) = i.reads.get(k)
                {
                    copy.insert(w, from);
                }
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for phi in p.blocks.iter().flat_map(|b| &b.phis) {
            let mut args = phi.args.iter().map(|&a| {
                let mut a = a;
                while let Some(&c) = copy.get(&a) {
                    a = c;
                }
                a
            });
            if let Some(first) = args.next()
                && first != phi.value
                && args.all(|a| a == first || a == phi.value)
                && copy.insert(phi.value, first).is_none()
            {
                changed = true;
            }
        }
    }
    let root = |mut v: Value| {
        // (copies form no cycle but through a phi of itself, left out above)
        for _ in 0..copy.len() + 1 {
            match copy.get(&v) {
                Some(&c) => v = c,
                None => break,
            }
        }
        v
    };
    let mut out = Vec::new();
    for d in lane_branches(st) {
        // the region: blocks reachable from the branch that do not post-dominate it
        let mut region = vec![false; n];
        let mut stack = succ[d].clone();
        while let Some(b) = stack.pop() {
            if region[b] || pdom[d][b] {
                continue;
            }
            region[b] = true;
            stack.extend(succ[b].iter().copied());
        }
        let last = p.blocks[d].insts.last();
        let on_exec = match last.map(|i| i.machine.name().unwrap_or("")) {
            Some("s_cbranch_execz" | "s_cbranch_execnz") => true,
            Some("s_cbranch_scc0" | "s_cbranch_scc1") => {
                // SCC of an EXEC write (`s_and_saveexec_b64`...): the mask is EXEC
                let scc = last.and_then(|i| i.read(Reg::Scc));
                p.blocks[d]
                    .insts
                    .iter()
                    .rev()
                    .find(|i| scc.is_some() && i.write(Reg::Scc) == scc)
                    .is_some_and(|i| i.write(Reg::S(EXEC)).is_some())
            }
            _ => false,
        };
        // (a branch on another mask, `s_and_b64 vcc, exec, m` then `s_cbranch_vccz`, is the
        // compiler's form of a branch it knows to be uniform: every active lane has the same
        // bit, and the per-lane form is the console's. That is the compiler's claim, not
        // checked here; a branch the compiler means for some lanes only goes by EXEC.)
        if !on_exec {
            continue;
        }
        // a loop closed by the branch: its block runs with the region
        let looped = succ[d].contains(&d) || (0..n).any(|b| region[b] && succ[b].contains(&d));
        let inside = |b: usize| region[b] || looped && b == d;
        // scalar values the region makes (not copies of values from before it: the compiler
        // parks a scalar in another SGPR and moves it back, the same value on every path)
        let made: HashSet<Value> = (0..n)
            .filter(|&b| inside(b))
            .flat_map(|b| {
                let block = &p.blocks[b];
                block.phis.iter().filter(|phi| scalar(phi.reg)).map(|phi| phi.value).chain(
                    block.insts.iter().flat_map(|i| i.writes.iter().filter(|w| scalar(w.0)).map(|w| w.1)),
                )
            })
            // (lane masks are lane data: a lane that did not run the region keeps its bit, as
            // a lane outside EXEC does on the console)
            .filter(|v| !mask(*v) && defined.get(&root(*v)).is_some_and(|&b| inside(b)))
            .map(root)
            .collect();
        let after = |b: usize| !inside(b) && b != d;
        let reads_made = |b: usize| p.blocks[b].insts.iter().any(|i| i.reads.iter().any(|r| made.contains(&root(r.1))));
        let joins_made = |b: usize| {
            let block = &p.blocks[b];
            block.phis.iter().filter(|phi| !mask(phi.value)).any(|phi| {
                let roots: HashSet<Value> = phi.args.iter().map(|&a| root(a)).collect();
                roots.len() > 1 && roots.iter().any(|r| made.contains(r))
            })
        };
        let read_after = (0..n).filter(|&b| after(b)).any(|b| reads_made(b) || joins_made(b));
        if read_after {
            if std::env::var_os("VICTORY_LANE_WHY").is_some() {
                let tail: Vec<String> = p.blocks[d].insts.iter().rev().take(3).map(|i| i.machine.text(false)).collect();
                let reader = (0..n)
                    .filter(|&b| after(b))
                    .flat_map(|b| &p.blocks[b].insts)
                    .find(|i| i.reads.iter().any(|r| made.contains(&root(r.1))))
                    .map(|i| i.machine.text(false));
                let phi = (0..n).filter(|&b| after(b) && joins_made(b)).map(|b| p.blocks[b].offset).next();
                eprintln!("LANE-WHY scalar after: {tail:?} read by {reader:?}, joined at {phi:x?}");
            }
            out.push(Inexact { block: d, why: SCALAR_AFTER });
        }
    }
    out
}

/// Every per-lane branch: not wave-wide, not on scalar conditions only.
pub fn lane_branches(st: &Structured) -> Vec<usize> {
    st.exits
        .iter()
        .enumerate()
        .filter_map(|(d, e)| match *e {
            LaneExit::Branch { when, .. }
                if !st.wide.contains(&d) && !st.bools.support(when).iter().all(|a| matches!(a, Atom::Uniform(_))) =>
            {
                Some(d)
            }
            _ => None,
        })
        .collect()
}

/// Per block, the blocks that post-dominate it (itself included).
fn post_dominators(succ: &[Vec<usize>]) -> Vec<Vec<bool>> {
    let n = succ.len();
    let mut pdom: Vec<Vec<bool>> = (0..n)
        .map(|b| if succ[b].is_empty() { (0..n).map(|c| c == b).collect() } else { vec![true; n] })
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..n).rev() {
            if succ[b].is_empty() {
                continue;
            }
            let mut v = vec![true; n];
            for &t in &succ[b] {
                for c in 0..n {
                    v[c] &= pdom[t][c];
                }
            }
            v[b] = true;
            if v != pdom[b] {
                pdom[b] = v;
                changed = true;
            }
        }
    }
    pdom
}
