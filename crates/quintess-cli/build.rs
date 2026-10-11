//! Embeds the icon and version details in the Windows executable, so
//! Explorer, the taskbar and file associations show Quintess's icon.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/quintess.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/quintess.ico");
        resource.set("FileDescription", "Quintess command-line PDF tool");
        resource.set("ProductName", "Quintess");
        if let Err(error) = resource.compile() {
            // A missing resource compiler only costs the icon, not the build.
            println!("cargo:warning=could not embed the icon: {error}");
        }
    }
}
