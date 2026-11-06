mod data;

pub(super) fn group(name: &[u8]) -> Option<(usize, bool)> {
    let mut canonical = [0; 128];
    let mut size = 0;
    for &byte in name {
        if matches!(byte, b'_' | b'-' | b' ') {
            continue;
        }
        if size == canonical.len() {
            return None;
        }
        *canonical.get_mut(size)? = byte.to_ascii_lowercase();
        size += 1;
    }
    let name = canonical.get(..size)?;
    match name {
        b"any" => Some((data::GROUPS.len(), false)),
        b"ascii" => Some((data::GROUPS.len() + 1, false)),
        b"assigned" => group(b"cn").map(|(index, _)| (index, true)),
        _ => data::NAMES
            .binary_search_by(|&(candidate, _)| candidate.as_bytes().cmp(name))
            .ok()
            .and_then(|index| data::NAMES.get(index).map(|&(_, group)| (group, false))),
    }
}

pub(super) fn contains(group: usize, point: u32) -> bool {
    if group == data::GROUPS.len() {
        return true;
    }
    if group == data::GROUPS.len() + 1 {
        return point < 128;
    }
    let Some(&(start, length)) = data::GROUPS.get(group) else {
        return false;
    };
    let Some(ranges) = data::RANGES.get(start..start + length) else {
        return false;
    };
    let index = ranges.partition_point(|&(low, _, _)| low <= point);
    index
        .checked_sub(1)
        .and_then(|i| ranges.get(i))
        .is_some_and(|&(low, high, stride)| point <= high && (point - low) % stride == 0)
}

pub(super) fn fold(point: u32) -> u32 {
    if let Ok(index) = data::ORBIT.binary_search_by_key(&point, |&(from, _)| from) {
        if let Some(&(_, next)) = data::ORBIT.get(index) {
            return next;
        }
    }
    let Some(rune) = char::from_u32(point) else {
        return point;
    };
    let lower = crate::casing::map(rune, false);
    if lower != rune {
        lower as u32
    } else {
        crate::casing::map(rune, true) as u32
    }
}

pub(super) fn folded(point: u32, enabled: bool, mut predicate: impl FnMut(u32) -> bool) -> bool {
    if predicate(point) {
        return true;
    }
    if enabled {
        let mut current = fold(point);
        while current != point {
            if predicate(current) {
                return true;
            }
            current = fold(current);
        }
    }
    false
}
