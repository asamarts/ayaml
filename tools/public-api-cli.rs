use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--version" => {
            println!("ayaml-public-api {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [cmd, json_path] if cmd == "list" => match list_public_api(PathBuf::from(json_path)) {
            Ok(items) => {
                for item in items {
                    println!("{item}");
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: ayaml-public-api --version | list RUSTDOC_JSON");
            ExitCode::from(2)
        }
    }
}

fn list_public_api(json_path: PathBuf) -> Result<BTreeSet<String>, public_api::Error> {
    let api = public_api::Builder::from_rustdoc_json(json_path)
        .omit_blanket_impls(true)
        .omit_auto_trait_impls(false)
        .omit_auto_derived_impls(false)
        .build()?;
    Ok(api.items().map(ToString::to_string).collect())
}
