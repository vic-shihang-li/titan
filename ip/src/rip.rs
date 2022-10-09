struct Entry {
    cost: u32,
    address: u32,
    mask: u32,
}

pub struct RipMessage {
    command: u16,
    entries: Vec<Entry>,
}
