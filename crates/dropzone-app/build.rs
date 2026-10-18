fn main() {
    // Embed the icon and version info into the Windows executable.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("embedding Windows resources");
    }
}
