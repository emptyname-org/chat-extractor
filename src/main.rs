use gtk::prelude::*;
use signal_filter::app::build_ui;

fn accessibility_socket_exists(runtime_directory: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(runtime_directory.join("at-spi")) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        if !entry.file_name().to_string_lossy().starts_with("bus_") {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt as _;
            entry
                .file_type()
                .ok()
                .is_some_and(|file_type| file_type.is_socket())
        }
        #[cfg(not(unix))]
        {
            true
        }
    })
}

fn configure_accessibility_backend() {
    if std::env::var_os("GTK_A11Y").is_some()
        || std::env::var("AT_SPI_BUS_ADDRESS").is_ok_and(|address| !address.is_empty())
    {
        return;
    }
    let available = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .is_some_and(|directory| accessibility_socket_exists(&directory));
    if !available {
        // SAFETY: Only single-threaded std queries run before GIO and GTK start.
        unsafe { std::env::set_var("GTK_A11Y", "none") };
    }
}

fn main() -> gtk::glib::ExitCode {
    configure_accessibility_backend();
    gtk::gio::resources_register_include!("signal-filter.gresource")
        .expect("embedded GTK resources must be valid");

    let application = gtk::Application::builder()
        .application_id("org.chatextractor.app")
        .build();
    application.connect_activate(build_ui);
    application.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_accessibility_socket_is_not_reported_as_available() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        assert!(!accessibility_socket_exists(directory.path()));
    }

    #[cfg(unix)]
    #[test]
    fn accessibility_socket_is_detected_without_starting_gio() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let at_spi = directory.path().join("at-spi");
        std::fs::create_dir(&at_spi).expect("at-spi directory should be created");
        let _listener = std::os::unix::net::UnixListener::bind(at_spi.join("bus_1"))
            .expect("test socket should bind");
        assert!(accessibility_socket_exists(directory.path()));
    }
}
