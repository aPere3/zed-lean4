mod proxy;
mod rpc;
mod state;
mod watch;

const USAGE: &str = "\
zed-lean4-companion: companion infoview for the Zed Lean 4 extension

USAGE:
  zed-lean4-companion proxy [--] <server command...>
      Run as the language server, proxying stdio to the real server
      (e.g. `zed-lean4-companion proxy -- lake serve --`). Publishes goal
      state on a unix socket.

  zed-lean4-companion watch
      Run in a terminal inside the worktree: connects to the proxy
      socket and displays the infoview live.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((mode, rest)) if mode == "proxy" => {
            let rest = match rest.first().map(String::as_str) {
                Some("--") => &rest[1..],
                _ => rest,
            };
            if rest.is_empty() {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
            proxy::run(rest.to_vec())
        }
        Some((mode, _)) if mode == "watch" => watch::run(),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("zed-lean4-companion: {e}");
        std::process::exit(1);
    }
}
