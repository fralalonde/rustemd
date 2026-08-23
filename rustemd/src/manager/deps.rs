//! Pure dependency-graph helpers over the unit table.
//!
//! These operate on the *declared* dependencies in each unit's `[Unit]`
//! section. Starting/stopping a unit needs: its start-closure (requires,
//! wants, requisite), its conflict closure (must be stopped first), the
//! After ordering names, and the reverse Requires edges for stop
//! propagation.

use std::collections::HashSet;

use crate::manager::state::Unit;

pub fn requires(u: &Unit) -> Vec<String> {
    let mut v = vec![];
    if let Some(f) = &u.file {
        let c = &f.unit;
        v.extend(c.requires.iter().cloned());
        v.extend(c.binds_to.iter().cloned());
    }
    v
}

pub fn wants(u: &Unit) -> Vec<String> {
    let mut v = vec![];
    if let Some(f) = &u.file {
        v.extend(f.unit.wants.iter().cloned());
    }
    v
}

pub fn requisite(u: &Unit) -> Vec<String> {
    let mut v = vec![];
    if let Some(f) = &u.file {
        v.extend(f.unit.requisite.iter().cloned());
    }
    v
}

pub fn conflicts(u: &Unit) -> Vec<String> {
    let mut v = vec![];
    if let Some(f) = &u.file {
        v.extend(f.unit.conflicts.iter().cloned());
    }
    v
}

pub fn after(u: &Unit) -> Vec<String> {
    let mut v = vec![];
    if let Some(f) = &u.file {
        v.extend(f.unit.after.iter().cloned());
    }
    v
}

pub fn on_failure(u: &Unit) -> Vec<String> {
    let mut v = vec![];
    if let Some(f) = &u.file {
        v.extend(f.unit.on_failure.iter().cloned());
    }
    v
}

/// BFS transitive closure over an edge selector.
fn closure<F>(units: &std::collections::HashMap<String, Unit>, start: &str, edge: F) -> Vec<String>
where
    F: Fn(&Unit) -> Vec<String>,
{
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue = vec![start.to_string()];
    let mut out = vec![];
    while let Some(n) = queue.pop() {
        let Some(u) = units.get(&n) else { continue };
        for d in edge(u) {
            if d == n {
                continue;
            }
            if seen.insert(d.clone()) {
                out.push(d.clone());
                queue.push(d);
            }
        }
    }
    out
}

/// All units reachable from `name` via requires/wants/requisite edges.
pub fn start_closure(
    units: &std::collections::HashMap<String, Unit>,
    name: &str,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    // needs = requires-like (fatal), weak = wants, requisite = must-already-active
    let needs: Vec<String> = closure(units, name, requires);
    let weak: Vec<String> = closure(units, name, wants);
    let reqs: Vec<String> = closure(units, name, requisite);
    (needs, weak, reqs)
}

/// All units reachable from `name` via the Conflicts= edge.
pub fn closure_conflicts(
    units: &std::collections::HashMap<String, Unit>,
    name: &str,
) -> Vec<String> {
    closure(units, name, conflicts)
}

/// Reverse Requires edges: units that will be stopped when `name` stops.
pub fn stop_propagate(units: &std::collections::HashMap<String, Unit>, name: &str) -> Vec<String> {
    // Any unit that requires/binds-to `name` gets stopped with it.
    let mut out = vec![];
    for (n, u) in units {
        if n == name {
            continue;
        }
        if requires(u).contains(&name.to_string()) {
            out.push(n.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::state::{ActiveState, Unit};
    use crate::unit::UnitKind;

    fn mk(name: &str, req: &[&str], want: &[&str], after: &[&str]) -> Unit {
        let mut u = Unit::new(name, UnitKind::Service);
        u.load = crate::manager::state::LoadState::Loaded;
        u.file = Some(crate::unit::UnitFile {
            path: None,
            unit: crate::unit::UnitConfig {
                requires: req.iter().map(|s| s.to_string()).collect(),
                wants: want.iter().map(|s| s.to_string()).collect(),
                after: after.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            service: None,
            timer: None,
            #[cfg(feature = "socket")]
            socket: None,
            #[cfg(target_os = "linux")]
            mount: None,
            install: Default::default(),
        });
        u
    }

    fn table(units: Vec<Unit>) -> std::collections::HashMap<String, Unit> {
        units.into_iter().map(|u| (u.name.clone(), u)).collect()
    }

    #[test]
    fn closure_transitive() {
        let t = table(vec![
            mk("a", &["b"], &[], &[]),
            mk("b", &["c"], &[], &[]),
            mk("c", &[], &[], &[]),
        ]);
        let (needs, _, _) = start_closure(&t, "a");
        assert!(needs.contains(&"b".to_string()));
        assert!(needs.contains(&"c".to_string()));
        let _ = ActiveState::Inactive;
    }

    #[test]
    fn stop_propagates_to_who_requires() {
        let t = table(vec![
            mk("app", &["db"], &[], &[]),
            mk("db", &[], &[], &[]),
            mk("other", &[], &[], &[]),
        ]);
        let to_stop = stop_propagate(&t, "db");
        assert_eq!(to_stop, vec!["app".to_string()]);
    }
}
