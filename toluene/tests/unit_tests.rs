#[cfg(test)]
mod tests {
    use toluene::add;

    #[test]
    fn test_add_positive() {
        assert_eq!(add(2, 3), 5);
        assert_eq!(add(0, 0), 0);
        assert_eq!(add(-1, 1), 0);
    }

    #[test]
    fn test_add_negative() {
        assert_eq!(add(-2, -3), -5);
    }

    // More tests for toluene crate's specific functionality as it evolves.
}
