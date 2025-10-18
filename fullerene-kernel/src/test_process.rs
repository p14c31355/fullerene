//! Test process module containing the test user process functions

// Test process main function
pub fn test_process_main() {
    // Use syscall helpers for reduced code duplication
    let message = b"Hello from test user process!\n";
    petroleum::write(1, message); // stdout fd = 1

    // Get and print PID
    let pid = petroleum::getpid();
    petroleum::write(1, b"My PID is: ");
    let pid_str = alloc::format!("{}\n", pid);
    petroleum::write(1, pid_str.as_bytes());

    // Yield twice for demonstration
    petroleum::sleep();
    petroleum::sleep();

    // Exit process
    petroleum::exit(0);
}
