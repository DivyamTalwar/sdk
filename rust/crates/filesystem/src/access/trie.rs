use super::compilation::{
    MAXIMUM_STATE_MEMBERS, MAXIMUM_STATES, MAXIMUM_TEXT_BYTES, MAXIMUM_TRANSITIONS, MAXIMUM_WORK,
    spend,
};
use super::glob::{Nfa, Symbol};
use super::{
    Action, Capability, Context, Decision, Denial, InvalidPolicy, Policy, Principal, Resource,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
struct Outcome {
    rule: Option<usize>,
    denial: Option<Denial>,
}

struct State {
    transitions: Vec<usize>,
    decisions: [Outcome; 12],
}

/// Determinized glob trie for one principal, volume, generation, and context.
/// Glob branches are resolved at compilation, never searched during a check.
/// Checks allocate nothing and visit one transition per UTF-8 scalar, independent
/// of rule count. The ASCII alphabet has direct lookup; Unicode literals use a
/// sorted alphabet. Storage is proportional to reachable deterministic states.
pub struct PathTrie {
    alphabet: Vec<char>,
    ascii: [usize; 128],
    states: Vec<State>,
    rules: Vec<String>,
    version: u64,
    valid_from: u64,
    /// Earliest time at which the caller must rebuild this compiled view.
    pub valid_until: Option<u64>,
}

impl PathTrie {
    /// Compile the same decision reducer used by `decide` into terminal states.
    /// `resource.path` is ignored; every other input is pinned for this view.
    /// Rejects policies whose glob automaton exceeds bounded state, storage or
    /// compilation work, including short patterns with exponential state growth.
    pub fn compile(
        policy: &Policy,
        principal: &Principal,
        resource: Resource<'_>,
        context: Context,
        capability: &Capability,
    ) -> Result<Self, InvalidPolicy> {
        policy.validate()?;
        capability.validate()?;
        if resource.generation > resource.head {
            return Err(InvalidPolicy("generation exceeds head"));
        }
        let mut nfa = Nfa::default();
        let mut starts = Vec::new();
        let mut rules = BTreeMap::new();
        let mut grants = BTreeMap::new();
        let mut valid_until = capability.expires_at.filter(|expiry| *expiry > context.now);
        for rule in policy.layers.iter().flatten() {
            if let Some(expiry) = rule
                .conditions
                .expires_at
                .filter(|expiry| *expiry > context.now)
            {
                valid_until = Some(valid_until.map_or(expiry, |old| old.min(expiry)));
            }
            if super::decision::rule_matches(rule, principal, resource, context) {
                let (start, end) = nfa.pattern(&rule.resource.path)?;
                starts.push(start);
                rules.insert(rule.id.as_str(), end);
            }
        }
        for grant in capability.layers.iter().flatten() {
            if grant.resource.matches_view(resource) {
                let (start, end) = nfa.pattern(&grant.resource.path)?;
                starts.push(start);
                grants.insert(std::ptr::from_ref(grant), end);
            }
        }
        let (alphabet, ascii, other) = alphabet(&nfa)?;
        let mut text_left = MAXIMUM_TEXT_BYTES;
        let rule_ids: Vec<_> = rules
            .keys()
            .map(|id| {
                text_left = text_left
                    .checked_sub(id.len())
                    .ok_or(InvalidPolicy("compiled rule ids exceed text limit"))?;
                Ok((*id).to_owned())
            })
            .collect::<Result<_, InvalidPolicy>>()?;
        let rule_index: BTreeMap<_, _> = rules
            .keys()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        let states = determinize(
            &nfa,
            starts,
            &alphabet,
            other,
            |subset| {
                Action::ALL.map(|action| {
                    let decision = super::decision::evaluate(
                        policy,
                        capability,
                        context,
                        action,
                        resource.generation != resource.head,
                        |rule| {
                            rules
                                .get(rule.id.as_str())
                                .is_some_and(|end| subset.binary_search(end).is_ok())
                        },
                        |grant| {
                            grants
                                .get(&std::ptr::from_ref(grant))
                                .is_some_and(|end| subset.binary_search(end).is_ok())
                        },
                    );
                    Outcome {
                        rule: decision.rule_id.and_then(|id| rule_index.get(id).copied()),
                        denial: decision.denial,
                    }
                })
            },
            policy.layers.len()
                + capability.layers.len()
                + policy.layers.iter().map(Vec::len).sum::<usize>()
                + capability.layers.iter().map(Vec::len).sum::<usize>(),
        )?;
        Ok(Self {
            alphabet,
            ascii,
            states,
            rules: rule_ids,
            version: policy.version,
            valid_from: context.now,
            valid_until,
        })
    }

    /// Decide one path without allocation. `now` prevents reuse after expiry,
    /// including expiry of a deny rule; the caller recompiles on an invalid view.
    #[must_use]
    pub fn decide(&self, action: Action, path: &str, now: u64) -> Decision<'_> {
        if now < self.valid_from
            || !super::glob::valid_path(path)
            || self.valid_until.is_some_and(|expiry| now >= expiry)
        {
            return Decision::denied(self.version, Denial::Invalid);
        }
        let mut state = 0;
        for ch in path.chars() {
            let symbol = if ch.is_ascii() {
                self.ascii
                    .get(ch as usize)
                    .copied()
                    .unwrap_or(self.alphabet.len())
            } else {
                self.alphabet
                    .binary_search(&ch)
                    .unwrap_or(self.alphabet.len())
            };
            let Some(next) = self
                .states
                .get(state)
                .and_then(|node| node.transitions.get(symbol))
            else {
                return Decision::denied(self.version, Denial::Invalid);
            };
            state = *next;
        }
        let outcome = self
            .states
            .get(state)
            .and_then(|node| node.decisions.get(action as usize))
            .copied();
        match outcome {
            Some(outcome) => Decision {
                allowed: outcome.denial.is_none(),
                rule_id: outcome
                    .rule
                    .and_then(|id| self.rules.get(id).map(String::as_str)),
                denial: outcome.denial,
                policy_version: self.version,
            },
            None => Decision::denied(self.version, Denial::Invalid),
        }
    }
}

fn alphabet(nfa: &Nfa) -> Result<(Vec<char>, [usize; 128], char), InvalidPolicy> {
    let mut alphabet: BTreeSet<char> = nfa
        .nodes
        .iter()
        .flat_map(|node| node.edges.iter())
        .filter_map(|(symbol, _)| match symbol {
            Symbol::Literal(c) => Some(*c),
            _ => None,
        })
        .collect();
    alphabet.insert('/');
    // One representative for every character not named literally.
    let other = (1..=0x10ffff)
        .filter_map(char::from_u32)
        .find(|ch| !alphabet.contains(ch))
        .ok_or(InvalidPolicy("exhausted glob alphabet"))?;
    let alphabet: Vec<_> = alphabet.into_iter().collect();
    let mut ascii = [alphabet.len(); 128];
    for (code, slot) in ascii.iter_mut().enumerate() {
        if let Ok(ch) = u8::try_from(code) {
            *slot = alphabet
                .binary_search(&char::from(ch))
                .unwrap_or(alphabet.len());
        }
    }

    Ok((alphabet, ascii, other))
}

fn determinize(
    nfa: &Nfa,
    mut starts: Vec<usize>,
    alphabet: &[char],
    other: char,
    decide: impl Fn(&[usize]) -> [Outcome; 12],
    rules: usize,
) -> Result<Vec<State>, InvalidPolicy> {
    let mut work = MAXIMUM_WORK;
    nfa.close(&mut starts, &mut work)?;
    let mut members = MAXIMUM_STATE_MEMBERS;
    spend(&mut members, starts.len())?;
    let mut transitions_left = MAXIMUM_TRANSITIONS;
    let mut ids = BTreeMap::from([(starts.clone(), 0)]);
    let mut subsets = vec![starts];
    let mut states = Vec::new();
    while states.len() < subsets.len() {
        spend(
            &mut work,
            rules
                .checked_mul(24)
                .ok_or(InvalidPolicy("glob compilation exceeds work limit"))?,
        )?;
        let subset = subsets
            .get(states.len())
            .ok_or(InvalidPolicy("invalid glob state"))?
            .clone();
        let decisions = decide(&subset);
        spend(&mut transitions_left, alphabet.len() + 1)?;
        let mut transitions = Vec::with_capacity(alphabet.len() + 1);
        for ch in alphabet.iter().copied().chain(std::iter::once(other)) {
            let next = nfa.step(&subset, ch, &mut work)?;
            let id = if let Some(id) = ids.get(&next) {
                *id
            } else {
                if subsets.len() == MAXIMUM_STATES {
                    return Err(InvalidPolicy("glob compilation exceeds DFA state limit"));
                }
                spend(&mut members, next.len())?;
                let id = subsets.len();
                ids.insert(next.clone(), id);
                subsets.push(next);
                id
            };
            transitions.push(id);
        }
        states.push(State {
            transitions,
            decisions,
        });
    }

    Ok(states)
}
