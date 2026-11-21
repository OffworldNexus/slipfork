//! The shipped help screens: one `-h` page per verb, no ANSI when colour is off.

use crate::render::paint;

use super::{HelpTopic, VERSION};

pub(super) fn help_text(topic: HelpTopic, color: bool) -> String {
    match topic {
        HelpTopic::Global => {
            let title = paint(&format!("slipfork {VERSION}"), "1;36", color);
            format!(
                "{title}\n\
                 Copy-on-write Btrfs environments for polyglot apps and AI coding agents.\n\
                 \n\
                 Usage: slipfork <command> [options]\n\
                 \n\
                 Commands:\n\
                 \x20 status    Show every running world as a tree (the bare `slipfork` verb)\n\
                 \x20 ps        Show only this directory's world or fork\n\
                 \x20 doctor    Check Btrfs, namespaces, and the crun + :80 runtime\n\
                 \x20 up        Boot the world declared by ./Forkfile and return when ready\n\
                 \x20 down      Stop this directory's running world\n\
                 \x20 restart   Restart one service, or every service\n\
                 \x20 fmt       Format a Forkfile in place\n\
                 \x20 migrate   Convert a legacy snow.yml into a Forkfile\n\
                 \x20 daemon    Install the per-user unit (and with sudo the system :80 socket)\n\
                 \x20 help      Print this overview\n\
                 \x20 version   Print the version\n\
                 \n\
                 Examples:\n\
                 \x20 slipfork\n\
                 \x20 slipfork up\n\
                 \x20 slipfork restart worker\n\
                 \x20 slipfork daemon install\n\
                 \n\
                 Options:\n\
                 \x20 -h, --help      Print this overview\n\
                 \x20 -V, --version   Print the version\n"
            )
        }
        HelpTopic::Status => {
            let title = paint("slipfork status", "1;36", color);
            format!(
                "{title}\n\
                 Show every running world as one tree; bare `slipfork` is this verb.\n\
                 \n\
                 Usage: slipfork status [--json] [-v]\n\
                 \n\
                 Options:\n\
                 \x20 --json        Emit the raw ps object with zero ANSI\n\
                 \x20 -v, --verbose Add each world's supervisor pid\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork status\n"
            )
        }
        HelpTopic::Ps => {
            let title = paint("slipfork ps", "1;36", color);
            format!(
                "{title}\n\
                 Show only the world or fork rooted at the current directory.\n\
                 \n\
                 Usage: slipfork ps [--json] [-v]\n\
                 \n\
                 Options:\n\
                 \x20 --json        Emit the raw ps object with zero ANSI\n\
                 \x20 -v, --verbose Add the supervisor pid\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork ps --json\n"
            )
        }
        HelpTopic::Doctor => {
            let title = paint("slipfork doctor", "1;36", color);
            format!(
                "{title}\n\
                 Check the only three host requirements: Btrfs, namespaces, and runtime.\n\
                 \n\
                 Usage: slipfork doctor [--json]\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit {{\"ready\",\"checks\"}} with zero ANSI\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork doctor\n"
            )
        }
        HelpTopic::DaemonInstall => {
            let title = paint("slipfork daemon install", "1;36", color);
            format!(
                "{title}\n\
                 Write the per-user daemon unit; `--ingress` adds the system :80 socket.\n\
                 \n\
                 Usage: slipfork daemon install [--ingress] [--json]\n\
                 \n\
                 Options:\n\
                 \x20 --ingress   Install slipfork-ingress.socket, its template unit, and AppArmor (sudo)\n\
                 \x20 --json      Emit {{\"installed\",\"ingress\"}} with zero ANSI\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 sudo slipfork daemon install --ingress\n"
            )
        }
        HelpTopic::Up => {
            let title = paint("slipfork up", "1;36", color);
            format!(
                "{title}\n\
                 Boot images, resources, setup, hooks, and services from ./Forkfile.\n\
                 \n\
                 Usage: slipfork up [--world NAME] [--json]\n\
                 \n\
                 Options:\n\
                 \x20 --world NAME   World name (default: the project directory name)\n\
                 \x20 --json         Stream typed boot events as NDJSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork up\n"
            )
        }
        HelpTopic::Down => {
            let title = paint("slipfork down", "1;36", color);
            format!(
                "{title}\n\
                 Stop this directory's running world and every pod in it.\n\
                 \n\
                 Usage: slipfork down [--json]\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit {{\"stopped\":true}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork down\n"
            )
        }
        HelpTopic::Restart => {
            let title = paint("slipfork restart", "1;36", color);
            format!(
                "{title}\n\
                 Restart one service from its existing bundle, or every service.\n\
                 \n\
                 Usage: slipfork restart [SERVICE] [--json]\n\
                 \n\
                 Arguments:\n\
                 \x20 SERVICE   Service id to restart (default: every service)\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit {{\"restarted\":N}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork restart worker\n"
            )
        }
        HelpTopic::Fmt => {
            let title = paint("slipfork fmt", "1;36", color);
            format!(
                "{title}\n\
                 Format a Forkfile in place, or report with `--check`.\n\
                 \n\
                 Usage: slipfork fmt [--check] [--json] [PATH]\n\
                 \n\
                 Arguments:\n\
                 \x20 PATH      Forkfile to format (default: Forkfile)\n\
                 \n\
                 Options:\n\
                 \x20 --check   Report whether PATH is formatted; exit 1 when not\n\
                 \x20 --json    Emit {{\"path\",\"formatted\",\"changed\"}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork fmt --check Forkfile\n"
            )
        }
        HelpTopic::Migrate => {
            let title = paint("slipfork migrate", "1;36", color);
            format!(
                "{title}\n\
                 Convert a legacy snow.yml manifest into a Forkfile.\n\
                 \n\
                 Usage: slipfork migrate [--write] [--force] [--json] [PATH]\n\
                 \n\
                 Arguments:\n\
                 \x20 PATH     snow.yml manifest to convert (default: snow.yml)\n\
                 \n\
                 Options:\n\
                 \x20 --write  Save the result as Forkfile next to PATH\n\
                 \x20 --force  Overwrite an existing Forkfile\n\
                 \x20 --json   Emit {{\"path\",\"written\",\"forkfile\"}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork migrate --write snow.yml\n"
            )
        }
    }
}
