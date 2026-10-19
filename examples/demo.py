#!/usr/bin/env python3
"""Scripted, replayable ezlo demo.

Spawns a throwaway zsh (with ezlo loaded) inside a pseudo-terminal, then "types" a short storyboard into it
with human-like timing while mirroring the screen to stdout. Nothing is faked: every dropdown, preview and
diagnostic is drawn live by ezlo against a fixture git repo created in a temp dir.

    ./demo.py                                   # watch it
    asciinema rec -c ./demo.py ezlo.cast        # record it (a ~100x30 terminal looks best)
    ./demo.py --scene rm --speed 2              # iterate on one scene, faster

Fully isolated: its own $HOME-less zsh config, ezlo config/specs/data/socket and history, all under a temp
dir that is removed on exit. Requires zsh, git and ezlo (on $PATH, $EZLO_BIN, or a release build under ../target or ../../target).
"""

from __future__ import annotations

import argparse
import fcntl
import os
import pty
import random
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent

# ── keys ────────────────────────────────────────────────────────────────────────────────────────────────
TAB = "\t"
ENTER = "\r"
DOWN = "\x1b[B"
ESC = "\x1b"
CTRL_U = "\x15"  # kill the whole line
NEXT_SCENE = "\x18\x0e"  # Ctrl-X Ctrl-N: demo widget that restores the prompt and clears the screen
CTRL_G = "\x07"  # ezlo pick
ALT_H = "\x1bh"  # ezlo explain

# ── fixture: a self-contained spec, so the demo doesn't depend on anyone's personal specs ──────────────
GIT_SPEC = r"""
type git_branch
  $ git for-each-ref --sort=-committerdate --format='%(refname:short)%09%(contents:subject)%09%(committerdate:relative)' refs/heads 2>/dev/null
  cols branch subject age
  insert branch
  show branch subject age
  preview `git log -n 20 --oneline --decorate --color=always "$branch"`
  cache 2s

type git_commit
  $ git log -n 100 --format='%h%x09%s%x09%cr' 2>/dev/null
  cols hash subject age
  insert hash
  show hash subject age
  preview `git show --stat --color=always "$hash"`
  cache 2s

cmd git switch "Switch branches"
  opt -c --create <new_branch> "Create a new branch and switch to it"
  opt -d --detach "Switch to a commit in detached HEAD"
  opt -f --discard-changes "Throw away local modifications"
  arg [branch] git_branch
  preview $ set -- {arg.branch}; [ $# -gt 0 ] || exit 0; git log -1 --format="$1 → %h %s (%cr)" "$1" -- 2>/dev/null

cmd git commit "Record changes to the repository"
  opt -m --message <msg> "Commit message"
  opt -a --all "Stage modified and deleted files before committing"
  opt --amend "Replace the tip of the current branch"
  opt --no-edit "Reuse the previous commit message"
  opt --fixup <commit> git_commit "Create a fixup! commit for <commit>"

cmd git "Distributed version control system"
  recipe "Undo recent commits, keeping the changes"
    $ git reset --soft <commit>
    arg <commit> git_commit

  recipe "Start a branch from another one"
    $ git switch -c <new_branch> <branch>
    arg <branch> git_branch

  recipe "Amend the last commit without editing its message"
    $ git commit --amend --no-edit
"""

EZLO_CONFIG = """
[sources]
enabled = ["user", "bundled"]   # deterministic: no native zsh fallback, no generated specs

[complete]
min_chars = 2
"""

# One prompt on screen at a time: after a command runs, the next prompt stays hidden (cursor too) until
# the driver starts the next scene with NEXT_SCENE, which restores it and clears the screen.
ZSHRC = r"""
unsetopt beep
setopt interactive_comments prompt_subst
HISTFILE="$EZLO_DEMO_ROOT/zsh_history"
HISTSIZE=1000 SAVEHIST=1000
export LS_COLORS= CLICOLOR=1 GIT_PAGER=cat PAGER=cat
_demo_branch() { local b; b=$(git branch --show-current 2>/dev/null) && [[ -n $b ]] && print -n "%F{8}($b)%f "; }
_demo_full=$'%{\e[?25h%}$(_demo_branch)%F{magenta}❯%f '
PROMPT=$_demo_full
RPROMPT=''
typeset -gi _demo_ran=0
_demo_preexec() { [[ $1 == '#'* ]] || _demo_ran=1 }  # caption comments keep the next prompt visible
_demo_precmd() { (( _demo_ran )) && PROMPT=$'%{\e[?25l%}' || PROMPT=$_demo_full; _demo_ran=0 }
_demo_next() { PROMPT=$_demo_full; zle clear-screen; zle reset-prompt }
# An accepted caption is redrawn without its prompt, in grey: it reads as a title, not as a second prompt.
_demo_finish() { [[ $BUFFER == '#'* ]] || return 0; PROMPT=''; region_highlight=("0 ${#BUFFER} fg=8"); zle reset-prompt }
autoload -Uz add-zsh-hook add-zle-hook-widget
add-zsh-hook preexec _demo_preexec
add-zsh-hook precmd _demo_precmd
zle -N _demo_next
zle -N _demo_finish
add-zle-hook-widget line-finish _demo_finish
bindkey '^X^N' _demo_next
eval "$(ezlo init zsh)"
"""

USERS_JSON = """{
  "users": [
    {"name": "Ada Lovelace", "email": "ada@acme.dev", "role": "admin"},
    {"name": "Alan Turing", "email": "alan@acme.dev", "role": "dev"},
    {"name": "Grace Hopper", "email": "grace@acme.dev", "role": "dev"}
  ],
  "total": 3
}
"""

# (branch, commit subject, age in seconds); the last one is where HEAD ends up.
BRANCHES: list[tuple[str, str, int]] = [
    ("release/1.4", "Bump version to 1.4.2", 21 * 86400),
    ("chore/deps", "Update dependencies", 6 * 86400),
    ("feature/login", "Add OAuth login flow", 2 * 86400),
    ("fix/throttle", "Fix off-by-one in rate limiter", 5 * 3600),
]


# ── storyboard ──────────────────────────────────────────────────────────────────────────────────────────
@dataclass
class Step:
    kind: str  # "type" | "keys" | "wait"
    value: str | float
    rate: float = 1.0  # typing speed multiplier (on top of --speed)


def type_(text: str, rate: float = 1.0) -> Step:
    return Step("type", text, rate)


def keys(seq: str) -> Step:
    return Step("keys", seq)


def wait(seconds: float) -> Step:
    return Step("wait", seconds)


def caption(text: str) -> list[Step]:
    """A `# comment` line that introduces a scene, typed twice as fast as commands and accepted as a no-op."""
    return [type_(f"# {text}", rate=2.0), wait(0.5), keys(ENTER), wait(0.3)]


SCENES: dict[str, list[Step]] = {
    # Live, rich candidates from a spec: branches with their last commit and age, plus a dry-run line.
    "switch": [
        *caption("use ezlo to suggest arguments"),
        type_("git switch "), wait(2.2),
        type_("feat"), wait(1.0),
        keys(TAB), wait(1.4),
        keys(ENTER), wait(1.4),
    ],
    # Typos are caught before Enter.
    "diagnose": [
        *caption("use ezlo to catch mistakes before you run them"),
        type_("git commit --amned -m 'fix login'"), wait(2.2),
        keys(CTRL_U), wait(0.4),
    ],
    # Alt-H: a word-by-word breakdown of the line (roles, value types, where suggestions come from).
    "explain": [
        *caption("use ezlo to explain commands (Alt-H)"),
        type_("git switch -c hotfix fix/throttle"), wait(1.0),
        keys(ALT_H), wait(4.0),
        keys(CTRL_U), wait(0.4),
    ],
    # Blast radius: what rm would delete, globs expanded, before running anything.
    "rm": [
        *caption("use ezlo to preview what a command will do"),
        type_("rm *.log"), wait(2.4),
        keys(CTRL_U), wait(0.4),
    ],
    # Spec-less pipeline smarts: jq paths read from the upstream file (accepting one also closes the quote).
    "jq": [
        *caption("use ezlo to complete data inside pipelines"),
        type_("cat users.json | jq '.us"), wait(1.6),
        keys(DOWN), wait(0.4), keys(DOWN), wait(0.4), keys(DOWN), wait(0.8),
        keys(TAB), wait(1.0),
        keys(ENTER if shutil.which("jq") else CTRL_U), wait(2.0),
    ],
    # Recipes: fuzzy-find a template, fill its placeholders from live data.
    "pick": [
        *caption("use ezlo to fill in command recipes (Ctrl-G)"),
        type_("git "), wait(0.6),
        keys(CTRL_G), wait(1.6),
        type_("undo"), wait(1.0),
        keys(ENTER), wait(1.6),
        keys(DOWN), wait(1.0),
        keys(ENTER), wait(2.6),
        keys(CTRL_U), wait(0.4),
    ],
}


# ── fixture setup ───────────────────────────────────────────────────────────────────────────────────────
def find_ezlo() -> str:
    # Release build: ../target in the standalone repo, ../../target in a Cargo workspace.
    candidates = [os.environ.get("EZLO_BIN"), shutil.which("ezlo"), *(str(p / "target/release/ezlo") for p in (HERE.parent, HERE.parent.parent))]
    for c in candidates:
        if c and os.access(c, os.X_OK):
            return str(Path(c).resolve())
    sys.exit("ezlo binary not found: put it on $PATH or set $EZLO_BIN")


def git(repo: Path, *args: str, age: int = 0) -> None:
    stamp = f"@{int(time.time()) - age} -0300"
    env = {**os.environ, "GIT_AUTHOR_DATE": stamp, "GIT_COMMITTER_DATE": stamp}
    subprocess.run(["git", "-C", str(repo), *args], check=True, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def build_repo(repo: Path) -> None:
    repo.mkdir(parents=True)
    (repo / ".gitignore").write_text("*.log\n")
    (repo / "users.json").write_text(USERS_JSON)
    (repo / "src").mkdir()
    (repo / "src/main.rs").write_text('fn main() {\n    println!("acme");\n}\n')
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "user.name", "Demo")
    git(repo, "config", "user.email", "demo@acme.dev")
    git(repo, "config", "commit.gpgsign", "false")
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "Initial commit", age=40 * 86400)
    for i, subject in enumerate(["Add user listing endpoint", "Cache user lookups", "Add request logging"]):
        (repo / "src/main.rs").open("a").write(f"// {subject}\n")
        git(repo, "commit", "-qam", subject, age=(30 - i * 4) * 86400)
    for branch, subject, age in BRANCHES:
        git(repo, "switch", "-qc", branch, "main")
        (repo / "src/main.rs").open("a").write(f"// {subject}\n")
        git(repo, "commit", "-qam", subject, age=age)
    git(repo, "switch", "-q", "main")
    # Ignored build logs for the blast-radius scene: 12 files, ~3.4 MB.
    rng = random.Random(7)
    for i in range(12):
        (repo / f"server-{i + 1:02d}.log").write_bytes(b"x" * rng.randint(200_000, 370_000))


def build_env(root: Path, ezlo: str) -> dict[str, str]:
    config_dir = root / "config"
    (config_dir / "specs").mkdir(parents=True)
    (config_dir / "config.toml").write_text(EZLO_CONFIG)
    (config_dir / "specs/git.ezlo").write_text(GIT_SPEC)
    zdotdir = root / "zdotdir"
    zdotdir.mkdir()
    (zdotdir / ".zshrc").write_text(ZSHRC)
    keep = {k: os.environ[k] for k in ("PATH", "HOME", "USER", "LANG", "LC_ALL", "TMPDIR") if k in os.environ}
    path = f"{Path(ezlo).parent}:{keep.get('PATH', '/usr/bin:/bin')}"
    return {
        **keep,
        "PATH": path,
        "TERM": os.environ.get("TERM", "xterm-256color"),
        "LANG": keep.get("LANG", "en_US.UTF-8"),
        "ZDOTDIR": str(zdotdir),
        "EZLO_BIN": ezlo,
        "EZLO_DEMO_ROOT": str(root),
        "EZLO_CONFIG_DIR": str(config_dir),
        "EZLO_SPECS_DIR": str(config_dir / "specs"),
        "EZLO_CACHE_DIR": str(root / "cache"),
        "EZLO_DATA_DIR": str(root / "data"),
        "EZLO_SOCKET": str(root / "ezlo.sock"),
    }


# ── pty driver ──────────────────────────────────────────────────────────────────────────────────────────
class Terminal:
    def __init__(self, cmd: list[str], env: dict[str, str], cwd: Path, size: tuple[int, int]) -> None:
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(cwd)
            os.execvpe(cmd[0], cmd, env)
        rows, cols = size
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def pump(self, seconds: float) -> None:
        """Mirror the child's output to stdout for `seconds`.

        The real terminal (asciinema's, tmux's, ...) answers queries the child prints, such as the cursor
        position report `ezlo pick` asks for (ESC[6n → ESC[row;colR). Those replies arrive on our stdin and
        are relayed to the child; anything else typed there is dropped so it can't derail the script.
        """
        stdin = sys.stdin.fileno() if sys.stdin.isatty() else None
        watch = [self.fd] if stdin is None else [self.fd, stdin]
        deadline = time.monotonic() + seconds
        while (left := deadline - time.monotonic()) > 0:
            ready, _, _ = select.select(watch, [], [], left)
            if stdin in ready:
                reply = os.read(stdin, 1024)
                if reply.startswith(b"\x1b["):
                    os.write(self.fd, reply)
            if self.fd not in ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            os.write(sys.stdout.fileno(), data)

    def send(self, data: str) -> None:
        os.write(self.fd, data.encode())

    def close(self) -> None:
        try:
            os.killpg(os.getpgid(self.pid), signal.SIGHUP)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass


def play(term: Terminal, steps: list[Step], speed: float, rng: random.Random) -> None:
    for step in steps:
        if step.kind == "wait":
            term.pump(float(step.value) / speed)
        elif step.kind == "keys":
            term.send(str(step.value))
            term.pump(0.05 / speed)
        else:
            for ch in str(step.value):
                term.send(ch)
                delay = rng.uniform(0.045, 0.11) * (2.2 if ch == " " else 1.0)
                term.pump(delay / (speed * step.rate))


def wait_for(path: Path, term: Terminal, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while not path.exists() and time.monotonic() < deadline:
        term.pump(0.05)


def quiet_stdin() -> Callable[[], None]:
    """Unbuffer stdin and stop keystrokes from echoing into the recording; returns the restore function."""
    if not sys.stdin.isatty():
        return lambda: None
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    quiet = termios.tcgetattr(fd)
    quiet[0] &= ~(termios.ICRNL | termios.IXON)
    quiet[3] &= ~(termios.ECHO | termios.ICANON | termios.IEXTEN)  # ISIG stays: Ctrl-C still aborts
    quiet[6][termios.VMIN] = 1
    quiet[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, quiet)
    return lambda: termios.tcsetattr(fd, termios.TCSANOW, saved)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    """Parse CLI flags: which scenes to play and how fast."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--scene", action="append", choices=list(SCENES), help="play only these scenes (repeatable)")
    parser.add_argument("--speed", type=float, default=1.0, help="playback speed multiplier")
    return parser.parse_args(argv)


def main() -> None:
    args = parse_args()

    ezlo = find_ezlo()
    for tool in ("zsh", "git"):
        if not shutil.which(tool):
            sys.exit(f"{tool} not found")

    size = shutil.get_terminal_size((100, 30))
    scenes = args.scene or list(SCENES)
    rng = random.Random(42)

    root = Path(tempfile.mkdtemp(prefix="ezlo-demo."))
    repo = root / "acme-api"
    restore = quiet_stdin()
    term: Terminal | None = None
    env: dict[str, str] = {}
    try:
        build_repo(repo)
        env = build_env(root, ezlo)
        sys.stdout.write("\x1b[2J\x1b[H")
        sys.stdout.flush()
        term = Terminal(["zsh", "-i"], env, repo, (size.lines, size.columns))
        wait_for(Path(env["EZLO_SOCKET"]), term, timeout=5)
        term.pump(0.8)
        for i, name in enumerate(scenes):
            if i:
                term.send(NEXT_SCENE)
                term.pump(0.5 / args.speed)
            play(term, SCENES[name], args.speed, rng)
        term.pump(1.0)
    finally:
        if term:
            term.close()
        if env:
            subprocess.run([ezlo, "daemon", "--stop", "--socket", env["EZLO_SOCKET"]],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env, timeout=5, check=False)
        restore()
        shutil.rmtree(root, ignore_errors=True)
        sys.stdout.write("\r\n")


if __name__ == "__main__":
    main()
