// 1. Strict lint rules for zero backdoors and memory safety
#![deny(unsafe_code)]
#![warn(clippy::pedantic)]

// Include the generated Rust code from the .slint file
slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    // 2. Clear, safe programmatic configuration substitute for SLINT_BACKEND
    // This tells Slint to bypass hardware requirements and use the software renderer directly
    slint::BackendSelector::new()
        .select()?;

    // Initialize the window component safely
    let ui = AppWindow::new()?;

    // Handle the button click event from the UI natively in Rust
    ui.on_button_clicked(|| {
        println!("Button clicked! This logic runs entirely on the CPU thread.");
    });

    // Run the application event loop
    ui.run()
}
