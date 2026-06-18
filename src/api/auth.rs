//! Constant-time bearer-token check for the read-only API.

/// True when `provided` equals `expected` without short-circuiting on the first
/// differing byte (avoids a timing side-channel on the shared secret).
pub fn token_matches(provided: &str, expected: &str) -> bool {
    let a = provided.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_on_exact_equality() {
        assert!(token_matches("secret-123", "secret-123"));
        assert!(!token_matches("secret-123", "secret-124"));
        assert!(!token_matches("secret", "secret-123"));
        assert!(!token_matches("", "secret"));
    }
}
