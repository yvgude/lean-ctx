use mock_instant::Instant;

pub fn stamp() -> u64 {
    0
}

pub fn hasher() -> ahash::RandomState {
    ahash::RandomState::with_seeds(1, 2, 3, 4)
}
