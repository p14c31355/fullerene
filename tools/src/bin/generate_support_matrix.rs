fn main() {
    let mut check = false;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--check" => check = true,
            "-h" | "--help" => {
                println!(
                    "Usage: generate-support-matrix [--check]\n\n\
                     Without --check, writes docs/SUPPORT_MATRIX.md."
                );
                return;
            }
            _ => {
                eprintln!("unknown argument: {argument}");
                std::process::exit(2);
            }
        }
    }

    if let Err(error) =
        fullerene_tools::generate_support_matrix(&fullerene_tools::workspace_root(), check)
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
