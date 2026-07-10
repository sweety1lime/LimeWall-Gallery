//! Per-user `.wpk` file association (HKCU\Software\Classes, no elevation):
//! double-clicking a package opens this UI with the file as an argument.

pub const PROG_ID: &str = "LimeWall.Package";

#[cfg(windows)]
fn map<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

#[cfg(windows)]
pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(map)?;
    let root = windows_registry::CURRENT_USER;

    let extension = root.create(r"Software\Classes\.wpk").map_err(map)?;
    extension.set_string("", PROG_ID).map_err(map)?;

    let progid = root
        .create(format!(r"Software\Classes\{PROG_ID}"))
        .map_err(map)?;
    progid
        .set_string("", "LimeWall wallpaper package")
        .map_err(map)?;

    let command = root
        .create(format!(r"Software\Classes\{PROG_ID}\shell\open\command"))
        .map_err(map)?;
    command
        .set_string("", format!("\"{}\" \"%1\"", exe.display()))
        .map_err(map)?;
    Ok(())
}

#[cfg(not(windows))]
pub fn register() -> Result<(), String> {
    Err("file association is Windows-only for now".into())
}

/// Removes the association; used by tests to leave the registry clean.
#[cfg(all(windows, test))]
pub fn unregister() -> Result<(), String> {
    let root = windows_registry::CURRENT_USER;
    root.remove_tree(r"Software\Classes\.wpk").map_err(map)?;
    root.remove_tree(format!(r"Software\Classes\{PROG_ID}"))
        .map_err(map)?;
    Ok(())
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;

    #[test]
    fn registers_extension_progid_and_command() {
        register().expect("register");

        let root = windows_registry::CURRENT_USER;
        let extension = root
            .open(r"Software\Classes\.wpk")
            .expect("extension key")
            .get_string("")
            .expect("extension value");
        assert_eq!(extension, PROG_ID);
        let command = root
            .open(format!(r"Software\Classes\{PROG_ID}\shell\open\command"))
            .expect("command key")
            .get_string("")
            .expect("command value");
        assert!(command.ends_with("\"%1\""));
        assert!(command.contains(".exe"));

        unregister().expect("cleanup");
        assert!(root.open(r"Software\Classes\.wpk").is_err());
    }
}
