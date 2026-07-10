#[cfg(not(windows))]
fn main() {
    eprintln!("display mode diagnostics are only available on Windows");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    windows_impl::run()
}

#[cfg(windows)]
mod windows_impl {
    use std::collections::BTreeSet;
    use std::mem::size_of;
    use std::time::Duration;

    use windows::Win32::Graphics::Gdi::{
        CDS_TEST, CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE,
        DISP_CHANGE_SUCCESSFUL, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE,
        EnumDisplaySettingsW,
    };
    use windows::core::PCWSTR;

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.as_slice() {
            [command, device] if command == "list" => list_modes(device),
            [command, device, width, height, seconds] if command == "cycle" => {
                let width = width.parse::<u32>()?;
                let height = height.parse::<u32>()?;
                let seconds = seconds.parse::<u64>()?;
                anyhow(seconds > 0 && seconds <= 30, "seconds must be 1-30")?;
                cycle_mode(device, width, height, Duration::from_secs(seconds))
            }
            _ => Err(
                "usage: display_mode list <device> | cycle <device> <width> <height> <seconds>"
                    .into(),
            ),
        }
    }

    fn list_modes(device: &str) -> Result<(), Box<dyn std::error::Error>> {
        let current = current_mode(device)?;
        println!(
            "current: {}x{} {} Hz {} bpp",
            current.dmPelsWidth,
            current.dmPelsHeight,
            current.dmDisplayFrequency,
            current.dmBitsPerPel
        );

        let mut unique = BTreeSet::new();
        let mut index = 0;
        while let Some(mode) = display_mode(device, ENUM_DISPLAY_SETTINGS_MODE(index)) {
            if mode.dmBitsPerPel == current.dmBitsPerPel {
                unique.insert((mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency));
            }
            index += 1;
        }
        for (width, height, frequency) in unique {
            println!("{width}x{height} {frequency} Hz");
        }
        Ok(())
    }

    fn cycle_mode(
        device: &str,
        width: u32,
        height: u32,
        duration: Duration,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let original = current_mode(device)?;
        anyhow(
            original.dmPelsWidth != width || original.dmPelsHeight != height,
            "requested mode is already active",
        )?;

        let target = find_compatible_mode(device, width, height, &original)
            .ok_or_else(|| format!("no compatible {width}x{height} mode found"))?;
        ensure_change_succeeds(device, &target, CDS_TEST, "mode test")?;

        let mut restore = RestoreGuard::new(device, original);
        ensure_change_succeeds(device, &target, CDS_TYPE(0), "mode switch")?;
        restore.active = true;
        println!(
            "active: {}x{} {} Hz for {} seconds",
            target.dmPelsWidth,
            target.dmPelsHeight,
            target.dmDisplayFrequency,
            duration.as_secs()
        );
        std::thread::sleep(duration);
        restore.restore()?;
        println!("restored original display mode");
        Ok(())
    }

    fn find_compatible_mode(
        device: &str,
        width: u32,
        height: u32,
        original: &DEVMODEW,
    ) -> Option<DEVMODEW> {
        let mut fallback = None;
        let mut index = 0;
        while let Some(mode) = display_mode(device, ENUM_DISPLAY_SETTINGS_MODE(index)) {
            if mode.dmPelsWidth == width
                && mode.dmPelsHeight == height
                && mode.dmBitsPerPel == original.dmBitsPerPel
            {
                if mode.dmDisplayFrequency == original.dmDisplayFrequency {
                    return Some(mode);
                }
                fallback = Some(mode);
            }
            index += 1;
        }
        fallback
    }

    fn current_mode(device: &str) -> Result<DEVMODEW, Box<dyn std::error::Error>> {
        display_mode(device, ENUM_CURRENT_SETTINGS)
            .ok_or_else(|| format!("failed to query current mode for {device}").into())
    }

    fn display_mode(device: &str, index: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
        let device = wide(device);
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        unsafe { EnumDisplaySettingsW(PCWSTR(device.as_ptr()), index, &mut mode) }
            .as_bool()
            .then_some(mode)
    }

    fn ensure_change_succeeds(
        device: &str,
        mode: &DEVMODEW,
        flags: CDS_TYPE,
        context: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let result = change_mode(device, mode, flags);
        if result == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(format!("{context} failed: {}", result.0).into())
        }
    }

    fn change_mode(device: &str, mode: &DEVMODEW, flags: CDS_TYPE) -> DISP_CHANGE {
        let device = wide(device);
        unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR(device.as_ptr()),
                Some(mode as *const DEVMODEW),
                None,
                flags,
                None,
            )
        }
    }

    struct RestoreGuard {
        device: String,
        original: DEVMODEW,
        active: bool,
    }

    impl RestoreGuard {
        fn new(device: &str, original: DEVMODEW) -> Self {
            Self {
                device: device.into(),
                original,
                active: false,
            }
        }

        fn restore(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            ensure_change_succeeds(&self.device, &self.original, CDS_TYPE(0), "display restore")?;
            self.active = false;
            Ok(())
        }
    }

    impl Drop for RestoreGuard {
        fn drop(&mut self) {
            if self.active {
                let result = change_mode(&self.device, &self.original, CDS_TYPE(0));
                if result != DISP_CHANGE_SUCCESSFUL {
                    eprintln!("emergency display restore failed: {}", result.0);
                }
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn anyhow(condition: bool, message: &str) -> Result<(), Box<dyn std::error::Error>> {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }
}
