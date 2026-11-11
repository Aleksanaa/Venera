use std::{env, fs, path::PathBuf};
use venera_debug_protocol::v1::{schema, Receiver};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    let check = match args.as_slice() {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => return Err("usage: generate-v1 [--check]".into()),
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut artifacts = Vec::new();
    for (name, receiver) in [("guest", Receiver::Guest), ("host", Receiver::Host)] {
        artifacts.push((
            root.join(format!("v1/{name}.openrpc.json")),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&schema::document(receiver))?
            ),
        ));
    }
    artifacts.push((
        root.join("../../docs/venera-protocol-v1.md"),
        schema::reference(),
    ));
    for (path, content) in artifacts {
        if check {
            if fs::read_to_string(&path)? != content {
                return Err(format!("generated artifact differs: {}", path.display()).into());
            }
            println!("verified {}", path.display());
        } else {
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(&path, content)?;
            println!("generated {}", path.display());
        }
    }
    Ok(())
}
