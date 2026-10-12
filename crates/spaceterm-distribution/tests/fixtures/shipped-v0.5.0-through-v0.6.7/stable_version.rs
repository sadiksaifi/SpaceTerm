pub(crate) fn stable_version(value: &str) -> Option<[u64; 3]> {
    if value.len() > 48 {
        return None;
    }
    let mut parts = value.split('.');
    let mut result = [0; 3];
    for number in &mut result {
        let part = parts.next()?;
        if part.is_empty()
            || part.len() > 1 && part.starts_with('0')
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        *number = part.parse().ok()?;
    }
    parts.next().is_none().then_some(result)
}
