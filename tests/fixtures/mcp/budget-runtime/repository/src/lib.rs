mod service;

pub fn budget_entry(value: usize) -> usize {
    service::transform(value) + budget_helper(value)
}

pub fn budget_helper(value: usize) -> usize {
    value.saturating_add(1)
}

pub fn budget_unused(value: usize) -> usize {
    value.saturating_mul(2)
}

#[cfg(test)]
mod tests {
    use super::budget_entry;

    #[test]
    fn entry_combines_bounded_helpers() {
        assert_eq!(budget_entry(2), 7);
    }
}
