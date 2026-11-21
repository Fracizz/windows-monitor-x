use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    for path in [
        "assets/windows-monitor.rc",
        "assets/windows-monitor.manifest",
        "assets/windows-monitor.ico",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    let project = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=WINDOWS_MONITOR_REPOSITORY_NAME");
    let repository = env::var("WINDOWS_MONITOR_REPOSITORY_NAME")
        .unwrap_or_else(|_| env!("CARGO_PKG_NAME").into());
    let base = repository
        .strip_suffix("-X")
        .or_else(|| repository.strip_suffix("-x"))
        .unwrap_or(&repository);
    let product_name = format!("{base}-x");
    assert!(
        !base.is_empty()
            && !base
                .chars()
                .any(|character| { character.is_control() || "<>:\"/\\|?*".contains(character) }),
        "repository name must form a valid Windows filename"
    );
    println!("cargo:rustc-env=WINDOWS_MONITOR_PRODUCT_NAME={product_name}");

    let version = env!("CARGO_PKG_VERSION");
    let version_fields = format!(
        "{},{},{},0",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH")
    );
    let manifest = out.join("windows-monitor.manifest");
    let manifest_text = include_str!("assets/windows-monitor.manifest")
        .replace("@ASSEMBLY_VERSION@", &version_fields.replace(',', "."));
    fs::write(&manifest, manifest_text).expect("write application manifest");

    let resource = out.join("windows-monitor.rc");
    let resource_text = include_str!("assets/windows-monitor.rc")
        .replace("@MANIFEST_PATH@", &rc_path(&manifest))
        .replace(
            "@ICON_PATH@",
            &rc_path(&project.join("assets/windows-monitor.ico")),
        )
        .replace("@VERSION_FIELDS@", &version_fields)
        .replace("@VERSION@", version)
        .replace("@PRODUCT_NAME@", &product_name);
    fs::write(&resource, resource_text).expect("write Windows version resource");

    embed_resource::compile(&resource, embed_resource::NONE)
        .manifest_required()
        .expect("compile Windows icon and application manifest");
}

fn rc_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
