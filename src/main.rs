#![windows_subsystem = "windows"]

#[cfg(windows)]
mod winui;

fn main() {
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).is_some_and(|a| a == "--benchmark") {
            if let Some(path) = args.get(2) {
                let results: Vec<_> = [1, 10_000, 5_010_000]
                    .into_iter()
                    .map(|ms| {
                        multi_point_clicker::engine::benchmark(
                            ms,
                            if ms == 5_010_000 { 10 } else { 500 },
                        )
                    })
                    .collect();
                if let Ok(json) = serde_json::to_vec_pretty(&results) {
                    let _ = std::fs::write(path, json);
                }
            }
            return;
        }
        if args.get(1).is_some_and(|a| a == "--input-probe") {
            if let Some(path) = args.get(2) {
                winui::input_probe(path);
            }
            return;
        }
        if args.get(1).is_some_and(|a| a == "--test-target") {
            if let Some(path) = args.get(2) {
                winui::test_target(path);
            }
            return;
        }
        winui::run();
    }
    #[cfg(not(windows))]
    eprintln!("Multi Point Clicker requires Windows.");
}
