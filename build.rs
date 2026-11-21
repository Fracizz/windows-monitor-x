fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        for path in [
            "assets/windows-monitor.rc",
            "assets/windows-monitor.manifest",
            "assets/windows-monitor.ico",
        ] {
            println!("cargo:rerun-if-changed={path}");
        }
        embed_resource::compile("assets/windows-monitor.rc", embed_resource::NONE)
            .manifest_required()
            .expect("compile Windows icon and application manifest");
    }
}
