#![forbid(unsafe_code)]

pub fn checked_increment(value: u32) -> Option<u32> {
    value.checked_add(1)
}
