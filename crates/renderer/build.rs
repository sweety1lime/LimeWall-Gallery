fn main() {
    #[cfg(target_os = "windows")]
    {
        // Embed the app icon so the exe, taskbar entries and the tray (which
        // extracts the first icon of its own module) all show the brand.
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/icon/limewall.ico");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=failed to embed the icon: {error}");
        }
        println!("cargo:rerun-if-changed=../../assets/icon/limewall.ico");
    }
}
