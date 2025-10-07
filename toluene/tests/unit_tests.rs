#[cfg(test)]
mod tests {
    #[test]
    fn test_simple_math() {
        assert_eq!(2 + 2, 4);
        assert_eq!(10 - 3, 7);
        assert_eq!(5 * 6, 30);
        assert_eq!(15 / 3, 5);
    }

    #[test]
    fn test_string_operations() {
        let s = "hello world";
        assert_eq!(s.len(), 11);
        assert!(s.contains("hello"));
        assert!(s.starts_with('h'));
    }

    #[test]
    fn test_vector_operations() {
        let mut v = vec![1, 2, 3];
        v.push(4);
        assert_eq!(v.len(), 4);
        assert_eq!(v[3], 4);
        assert_eq!(v.iter().sum::<i32>(), 10);
    }

    #[test]
    fn test_option_some() {
        let opt = Some(42);
        assert!(opt.is_some());
        assert_eq!(opt.unwrap(), 42);
    }

    #[test]
    fn test_option_none() {
        let opt: Option<i32> = None;
        assert!(opt.is_none());
        // This test ensures None handling doesn't panic
        let result = opt.unwrap_or(0);
        assert_eq!(result, 0);
    }
}
