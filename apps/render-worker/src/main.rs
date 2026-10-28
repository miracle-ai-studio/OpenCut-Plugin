fn main() {
    if std::env::args_os().len() != 1 || opencut_native_services::native_visual::serve().is_err() {
        std::process::exit(1);
    }
}
