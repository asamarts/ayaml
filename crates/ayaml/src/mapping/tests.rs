use super::{Mapping, SMALL_LIMIT};
use crate::value::Value;

fn key(name: &str) -> Value {
    Value::String(name.to_owned())
}

#[test]
fn insertion_order_is_preserved() {
    let mut map = Mapping::new();
    map.insert(key("a"), Value::from(1_u64));
    map.insert(key("b"), Value::from(2_u64));
    map.insert(key("c"), Value::from(3_u64));
    let keys: Vec<_> = map.keys().cloned().collect();
    assert_eq!(keys, vec![key("a"), key("b"), key("c")]);
}

#[test]
fn replacement_does_not_move_the_key() {
    let mut map = Mapping::new();
    map.insert(key("a"), Value::from(1_u64));
    map.insert(key("b"), Value::from(2_u64));
    assert_eq!(
        map.insert(key("a"), Value::from(3_u64)),
        Some(Value::from(1_u64))
    );
    let keys: Vec<_> = map.keys().cloned().collect();
    assert_eq!(keys, vec![key("a"), key("b")]);
}

#[test]
fn borrowed_string_lookups_match_value_keys() {
    let mut map = Mapping::new();
    map.insert(key("a"), Value::from(1_u64));
    map.insert(key("b"), Value::from(2_u64));
    assert!(map.contains_key("a"));
    assert!(map.get("a").is_some());
    assert_eq!(map.get("b"), Some(&Value::from(2_u64)));
    assert_eq!(map.get_str("a"), Some(&Value::from(1_u64)));
    *map.get_str_mut("a").expect("mutable lookup") = Value::from(3_u64);
    *map.get_mut("b").expect("generic mutable lookup") = Value::from(4_u64);
    assert_eq!(map.remove("a"), Some(Value::from(3_u64)));
    assert_eq!(map.shift_remove("b"), Some(Value::from(4_u64)));
    assert!(map.is_empty());
}

#[test]
fn remove_uses_swap_remove() {
    let mut map = Mapping::new();
    for name in ["a", "b", "c", "d"] {
        map.insert(key(name), Value::from(name.len() as u64));
    }
    map.remove(key("b"));
    let keys: Vec<_> = map.keys().cloned().collect();
    assert_eq!(keys, vec![key("a"), key("d"), key("c")]);
}

#[test]
fn shift_remove_preserves_order() {
    let mut map = Mapping::new();
    for name in ["a", "b", "c"] {
        map.insert(key(name), Value::Null);
    }
    map.shift_remove(key("b"));
    let keys: Vec<_> = map.keys().cloned().collect();
    assert_eq!(keys, vec![key("a"), key("c")]);
}

#[test]
fn mapping_equality_and_hash_are_order_insensitive() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut left = Mapping::new();
    left.insert(key("a"), Value::from(1_u64));
    left.insert(key("b"), Value::from(2_u64));

    let mut right = Mapping::new();
    right.insert(key("b"), Value::from(2_u64));
    right.insert(key("a"), Value::from(1_u64));

    assert_eq!(left, right);
    let mut left_hash = DefaultHasher::new();
    let mut right_hash = DefaultHasher::new();
    left.hash(&mut left_hash);
    right.hash(&mut right_hash);
    assert_eq!(left_hash.finish(), right_hash.finish());
}

#[test]
fn small_to_large_promotion_and_shrink_preserve_semantics() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn digest(map: &Mapping) -> u64 {
        let mut hasher = DefaultHasher::new();
        map.hash(&mut hasher);
        hasher.finish()
    }

    let mut map = Mapping::new();
    for idx in 0..=(SMALL_LIMIT + 4) {
        assert_eq!(
            map.insert(key(&format!("k{idx}")), Value::from(idx as u64)),
            None
        );
    }
    assert_eq!(map.len(), SMALL_LIMIT + 5);
    for idx in 0..=(SMALL_LIMIT + 4) {
        assert_eq!(map.get(format!("k{idx}")), Some(&Value::from(idx as u64)));
    }
    // Removing back below SMALL_LIMIT and shrinking converts cleanly back to exact small storage.
    for idx in 3..=(SMALL_LIMIT + 4) {
        assert_eq!(
            map.shift_remove(format!("k{idx}")),
            Some(Value::from(idx as u64))
        );
    }
    let mut fresh_small = Mapping::new();
    for idx in 0_u64..3 {
        fresh_small.insert(key(&format!("k{idx}")), Value::from(idx));
    }
    assert_eq!(map, fresh_small);
    assert_eq!(digest(&map), digest(&fresh_small));
    map.shrink_to_fit();
    assert_eq!(map.capacity(), 3);
    assert_eq!(map, fresh_small);
    assert_eq!(digest(&map), digest(&fresh_small));

    // Double-ended IntoIter, nth, nth_back, and Clone work across small maps.
    let iter = map.into_iter();
    let mut cloned = iter.clone();
    assert_eq!(cloned.len(), 3);
    assert_eq!(cloned.next_back(), Some((key("k2"), Value::from(2_u64))));
    assert_eq!(cloned.nth(1), Some((key("k1"), Value::from(1_u64))));
    assert_eq!(cloned.next(), None);
    let collected: Vec<_> = iter.collect();
    assert_eq!(collected.len(), 3);
}

#[test]
fn entry_inserts_lazily_and_mutates_existing() {
    let mut map = Mapping::new();
    let _slot = map.entry(key("a")).or_insert(Value::from(1_u64));
    let _slot = map
        .entry(key("a"))
        .and_modify(|value| *value = Value::from(2_u64));
    let _slot = map.entry(key("b")).or_insert_with(|| Value::from(3_u64));
    assert_eq!(map.get("a"), Some(&Value::from(2_u64)));
    assert_eq!(map.get("b"), Some(&Value::from(3_u64)));
}

#[test]
fn entry_implements_debug() {
    let mut map = Mapping::new();
    let entry = map.entry(key("a"));
    let rendered = format!("{entry:?}");
    assert_ne!(rendered, "");
}

#[test]
fn debug_prints_the_mapping_name_and_entries() {
    assert_eq!(format!("{:?}", Mapping::new()), "Mapping {}");
    let mut map = Mapping::new();
    map.insert(key("a"), Value::from(1_u64));
    assert_eq!(
        format!("{map:?}"),
        "Mapping {String(\"a\"): Number(Number(PosInt(1)))}"
    );
}

/// A mapping caches its hash, and every way to change what it holds forgets the hash: each
/// row changes a mapping that has been hashed, and it must then hash as a mapping built
/// anew from the same entries does. Every row changes the entries, or the cache could stay.
#[test]
fn cached_hash_tracks_every_mutation() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn digest(map: &Mapping) -> u64 {
        let mut hasher = DefaultHasher::new();
        map.hash(&mut hasher);
        hasher.finish()
    }

    type Change = fn(&mut Mapping);
    let changes: [(&str, Change); 22] = [
        ("insert a new key", |map| {
            map.insert(key("z"), Value::from(9_u64));
        }),
        ("insert over a key", |map| {
            map.insert(key("a"), Value::from(9_u64));
        }),
        ("insert_new", |map| {
            map.insert_new(key("z"), Value::from(9_u64)).expect("new");
        }),
        ("get_mut by str", |map| {
            *map.get_mut("a").expect("a") = Value::from(9_u64);
        }),
        ("get_mut by value", |map| {
            let wanted = key("a");
            *map.get_mut(&wanted).expect("a") = Value::from(9_u64);
        }),
        ("get_str_mut", |map| {
            *map.get_str_mut("a").expect("a") = Value::from(9_u64);
        }),
        ("entry or_insert, absent", |map| {
            let _ = map.entry(key("z")).or_insert(Value::from(9_u64));
        }),
        ("entry or_insert, present, written through", |map| {
            *map.entry(key("a")).or_insert(Value::Null) = Value::from(9_u64);
        }),
        ("entry or_insert_with, absent", |map| {
            map.entry(key("z")).or_insert_with(|| Value::from(9_u64));
        }),
        ("entry or_insert_with, present, written through", |map| {
            *map.entry(key("a")).or_insert_with(|| Value::Null) = Value::from(9_u64);
        }),
        ("entry and_modify", |map| {
            drop(
                map.entry(key("a"))
                    .and_modify(|value| *value = Value::from(9_u64)),
            );
        }),
        ("remove by str", |map| {
            map.remove("a").expect("a");
        }),
        ("remove by value", |map| {
            let wanted = key("a");
            map.remove(&wanted).expect("a");
        }),
        ("shift_remove by str", |map| {
            map.shift_remove("a").expect("a");
        }),
        ("shift_remove by value", |map| {
            let wanted = key("a");
            map.shift_remove(&wanted).expect("a");
        }),
        ("iter_mut", |map| {
            for (_, value) in map.iter_mut() {
                *value = Value::from(9_u64);
            }
        }),
        ("a loop over &mut", |map| {
            for (_, value) in &mut *map {
                *value = Value::from(9_u64);
            }
        }),
        ("values_mut", |map| {
            for value in map.values_mut() {
                *value = Value::from(9_u64);
            }
        }),
        ("extend", |map| {
            map.extend([(key("z"), Value::from(9_u64))]);
        }),
        ("clear", Mapping::clear),
        ("a value's index, absent", |map| {
            let mut value = Value::Mapping(std::mem::take(map));
            value["z"] = Value::from(9_u64);
            let Value::Mapping(changed) = value else {
                unreachable!()
            };
            *map = changed;
        }),
        ("a value's index, present", |map| {
            let mut value = Value::Mapping(std::mem::take(map));
            value["a"] = Value::from(9_u64);
            let Value::Mapping(changed) = value else {
                unreachable!()
            };
            *map = changed;
        }),
    ];
    // Exercise both small-map and promoted large-map representations.
    for extra in [0_usize, SMALL_LIMIT] {
        for (name, change) in changes {
            let mut map = Mapping::new();
            map.insert(key("a"), Value::from(1_u64));
            map.insert(key("b"), Value::from(2_u64));
            for idx in 0..extra {
                map.insert(key(&format!("extra{idx}")), Value::from(idx as u64));
            }
            let before = digest(&map);
            // Hashed again, it gives the cached hash; the change has to forget that.
            assert_eq!(digest(&map), before, "{name} (extra={extra})");
            change(&mut map);
            let fresh: Mapping = map
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            assert_ne!(
                digest(&fresh),
                before,
                "{name} (extra={extra}) changes nothing"
            );
            assert_eq!(digest(&map), digest(&fresh), "{name} (extra={extra})");
        }
    }
    // Giving back spare room changes no entry, and may keep the hash.
    let mut map = Mapping::new();
    map.insert(key("a"), Value::from(1_u64));
    let before = digest(&map);
    map.shrink_to_fit();
    assert_eq!(digest(&map), before);
}

/// Without a mark for each kind of number, these write the same bytes, and a mapping of them
/// cancels out under any key.
#[test]
fn what_values_write_tells_them_apart() {
    use std::hash::{Hash, Hasher};

    #[derive(Default)]
    struct Bytes(Vec<u8>);
    impl Hasher for Bytes {
        fn write(&mut self, bytes: &[u8]) {
            self.0.extend_from_slice(bytes);
        }
        fn finish(&self) -> u64 {
            0
        }
    }
    let bytes = |values: &[&str]| {
        let mut bytes = Bytes::default();
        for value in values {
            crate::parse_value(value).expect("a value").hash(&mut bytes);
        }
        bytes.0
    };
    let max = u64::MAX.to_string();
    let wide = ((2_u128 << 64) + 1).to_string();
    let high = ((1_u128 << 127) + 5).to_string();
    let low = format!("-{}", (1_u128 << 127) - 5);
    for (left, right) in [
        (vec!["-1"], vec![max.as_str()]),
        (vec!["1", "0"], vec![wide.as_str(), "~"]),
        (vec!["[-1, -1]"], vec!["[-1, 18446744073709551615]"]),
        (vec![high.as_str()], vec![low.as_str()]),
        (vec!["1.5"], vec!["4609434218613702656"]),
    ] {
        assert_ne!(bytes(&left), bytes(&right), "{left:?} and {right:?}");
    }
}

/// Any 65 values of 64 bits have a subset whose XOR is zero. Found by elimination under a
/// key anyone can compute, the subset splits into two mappings that would hash alike, and
/// enough of those are keys that take quadratic time.
#[test]
fn entries_cannot_be_chosen_to_cancel_out() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash as _, Hasher as _};
    let entry = |index: u32| {
        let mut hasher = DefaultHasher::new();
        key(&format!("e{index}")).hash(&mut hasher);
        Value::Null.hash(&mut hasher);
        hasher.finish()
    };
    let mut basis: Vec<(u64, u128)> = Vec::new();
    let mut subset = None;
    for index in 0..65 {
        let (mut value, mut members) = (entry(index), 1_u128 << index);
        for (reduced, its_members) in &basis {
            if value ^ reduced < value {
                value ^= reduced;
                members ^= its_members;
            }
        }
        if value == 0 {
            subset = Some(members);
            break;
        }
        basis.push((value, members));
        basis.sort_unstable_by_key(|(reduced, _)| std::cmp::Reverse(*reduced));
    }
    let subset = subset.expect("65 values are dependent");
    let mut sides = [Mapping::new(), Mapping::new()];
    let mut count = 0;
    for index in 0..65 {
        if subset & (1 << index) != 0 {
            sides[count % 2].insert(key(&format!("e{index}")), Value::Null);
            count += 1;
        }
    }
    let hash = |map: &Mapping| {
        let mut hasher = DefaultHasher::new();
        map.hash(&mut hasher);
        hasher.finish()
    };
    assert_ne!(sides[0], sides[1]);
    assert_ne!(hash(&sides[0]), hash(&sides[1]));
}

#[test]
fn missing_index_is_null() {
    let map = Mapping::new();
    assert_eq!(map["missing"], Value::Null);
}

#[test]
fn small_empty_and_growth_and_into_iter_edges_match_empty_and_large() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash as _, Hasher as _};

    let hash_of = |map: &Mapping| {
        let mut hasher = DefaultHasher::new();
        map.hash(&mut hasher);
        hasher.finish()
    };

    let empty = Mapping::new();
    let small_prealloc = Mapping::with_capacity(4);
    let mut small_cleared = Mapping::with_capacity(4);
    small_cleared.insert(key("a"), Value::from(1_i64));
    small_cleared.clear();
    let mut small_removed = Mapping::with_capacity(4);
    small_removed.insert(key("a"), Value::from(1_i64));
    assert_eq!(small_removed.remove("a"), Some(Value::from(1_i64)));
    let mut large_cleared = Mapping::with_capacity(16);
    large_cleared.insert(key("a"), Value::from(1_i64));
    large_cleared.clear();

    for mut candidate in [small_prealloc, small_cleared, small_removed, large_cleared] {
        assert!(candidate.is_empty());
        assert_eq!(candidate.len(), 0);
        assert_eq!(candidate, empty);
        assert_eq!(empty, candidate);
        assert_eq!(hash_of(&candidate), hash_of(&empty));
        assert_eq!(candidate.iter().count(), 0);
        assert_eq!(candidate.clone().into_iter().count(), 0);
        candidate.shrink_to_fit();
        assert_eq!(candidate, empty);
        assert_eq!(hash_of(&candidate), hash_of(&empty));
    }

    // Growth chain from with_capacity(1): 1 -> 2 -> 4 -> 8 -> Large.
    let mut growing = Mapping::with_capacity(1);
    for idx in 0..12_i64 {
        assert_eq!(
            growing.insert(key(&format!("k{idx}")), Value::from(idx)),
            None
        );
    }
    assert_eq!(growing.len(), 12);
    for idx in 0..12_i64 {
        assert_eq!(growing.get(format!("k{idx}")), Some(&Value::from(idx)));
    }

    // IntoIter::nth_back and partial consumption + drop when len < cap.
    let mut partial = Mapping::with_capacity(8);
    for idx in 0..5_i64 {
        partial.insert(key(&format!("p{idx}")), Value::from(idx));
    }
    let mut iter = partial.into_iter();
    assert_eq!(iter.nth_back(1), Some((key("p3"), Value::from(3_i64))));
    assert_eq!(iter.len(), 3);
    assert_eq!(iter.next(), Some((key("p0"), Value::from(0_i64))));
    assert_eq!(iter.nth_back(usize::MAX), None);
    assert_eq!(iter.len(), 0);
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);

    // Partial drop leaves no leaks or panics under Miri.
    let mut dropped = Mapping::with_capacity(8);
    for idx in 0..4_i64 {
        dropped.insert(key(&format!("d{idx}")), Value::from(format!("val-{idx}")));
    }
    let mut iter = dropped.into_iter();
    assert_eq!(iter.next().map(|(k, _)| k), Some(key("d0")));
    assert_eq!(iter.next_back().map(|(k, _)| k), Some(key("d3")));
    drop(iter);

    // IntoIter::collect after partial consumption (front > 0, back < cap) and exhaustion.
    let mut partial_collect = Mapping::with_capacity(8);
    for idx in 0..5_i64 {
        partial_collect.insert(key(&format!("c{idx}")), Value::from(idx));
    }
    let mut iter = partial_collect.into_iter();
    assert_eq!(iter.next(), Some((key("c0"), Value::from(0_i64))));
    assert_eq!(iter.next_back(), Some((key("c4"), Value::from(4_i64))));
    let remaining: Vec<_> = iter.collect();
    assert_eq!(
        remaining,
        vec![
            (key("c1"), Value::from(1_i64)),
            (key("c2"), Value::from(2_i64)),
            (key("c3"), Value::from(3_i64)),
        ]
    );
    let mut exhausted = Mapping::with_capacity(4);
    exhausted.insert(key("only"), Value::from(1_i64));
    let mut iter = exhausted.into_iter();
    assert_eq!(iter.next(), Some((key("only"), Value::from(1_i64))));
    let rest: Vec<_> = iter.collect();
    assert_eq!(rest.len(), 0);

    // Entry::Debug across SmallVacant, SmallOccupied, and Large, plus Iter/IterMut fold/rfold/collect.
    for size in [3_u64, (SMALL_LIMIT + 2) as u64] {
        let mut map = Mapping::new();
        for idx in 0..size {
            map.insert(key(&format!("e{idx}")), Value::from(idx));
        }
        let occ = format!("{:?}", map.entry(key("e0")));
        assert!(occ.contains("Occupied"), "occ={occ}");
        let vac = format!("{:?}", map.entry(key("absent")));
        assert!(vac.contains("Vacant"), "vac={vac}");
        let sum_fwd = map
            .iter()
            .fold(0_u64, |acc, (_, v)| acc + v.as_u64().expect("u64"));
        let sum_rev = map
            .iter()
            .rfold(0_u64, |acc, (_, v)| acc + v.as_u64().expect("u64"));
        assert_eq!(sum_fwd, sum_rev);
        let pairs: Vec<_> = map.iter().collect();
        assert_eq!(pairs.len() as u64, size);
        let bumped = map.iter_mut().rfold(0_u64, |acc, (_, v)| {
            let next = v.as_u64().expect("u64") + 1;
            *v = Value::from(next);
            acc + next
        });
        let collected_mut: Vec<_> = map.iter_mut().collect();
        assert_eq!(collected_mut.len() as u64, size);
        assert_eq!(bumped, sum_fwd + size);
    }
}
