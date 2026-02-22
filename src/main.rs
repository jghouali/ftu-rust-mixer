mod alsa_backend;
mod app;
mod config;
mod models;
mod presets;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use eframe::{NativeOptions, Renderer};

use crate::app::MixerApp;

#[derive(Parser, Debug)]
#[command(author, version, about = "Fast Track Ultra mixer for Linux")]
struct Args {
    /// ALSA card index to use, e.g. 2 for hw:2
    #[arg(long)]
    card: Option<u32>,

    /// JSON preset to load on startup
    #[arg(long)]
    load_preset: Option<String>,

    /// Graphics renderer: wgpu (default) or glow
    #[arg(long, value_enum, default_value_t = RenderMode::Wgpu)]
    render_mode: RenderMode,

}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum RenderMode {
    Wgpu,
    Glow,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let app = MixerApp::bootstrap(args.card, args.load_preset.as_deref())?;
    let renderer = pick_renderer(args.render_mode);

    let native_options = NativeOptions {
        renderer,
        ..Default::default()
    };
    eframe::run_native(
        "Fast Track Ultra Mixer (Rust)",
        native_options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
    .map_err(|e| anyhow::anyhow!("Failed to run GUI: {e}"))?;

    Ok(())
}

fn pick_renderer(render_mode: RenderMode) -> Renderer {
    match render_mode {
        RenderMode::Wgpu => Renderer::Wgpu,
        RenderMode::Glow => Renderer::Glow,
    }
}
