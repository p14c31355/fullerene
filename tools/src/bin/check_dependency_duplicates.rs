fn main() {
    if let Err(error) =
        fullerene_tools::check_dependency_duplicates(&fullerene_tools::workspace_root())
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
