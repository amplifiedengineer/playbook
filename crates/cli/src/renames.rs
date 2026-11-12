// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Old command paths that still work and the argv rewrite behind them.

use std::ffi::OsString;

/// One renamed command: `old` and `new` are space joined paths after the
/// program name, and `extra` is appended after the new words.
#[derive(Debug, Clone, Copy)]
pub struct Rename {
    pub old: &'static str,
    pub new: &'static str,
    pub extra: &'static [&'static str],
}

/// Every renamed command path.
pub const COMMAND_RENAMES: &[Rename] = &[Rename {
    old: "deliver",
    new: "pr create",
    extra: &[],
}];

/// Hidden legacy commands that keep their own code and output but print the
/// deprecation line: the argv is not rewritten.
pub const LEGACY_NOTICES: &[Rename] = &[
    Rename {
        old: "pr ci-wait",
        new: "pr merge",
        extra: &[],
    },
    Rename {
        old: "pr land-wait",
        new: "pr merge",
        extra: &[],
    },
];

/// The release that removes the old names.
pub const ALIAS_REMOVAL: &str = "1.0.0";

/// The stderr line for a deprecated command path.
pub fn deprecation_line(old: &str, new: &str) -> String {
    format!("playbook {old} is renamed, use: playbook {new} (the old name is removed in {ALIAS_REMOVAL})")
}

/// The stderr line for a config key that was renamed.
pub fn key_alias_line(old: &str, new: &str) -> String {
    format!("config key {old} is renamed, use: {new} (the old name is removed in {ALIAS_REMOVAL})")
}

/// The line for `given` when it is an old name of `canonical`.
pub fn key_notice(given: &str, canonical: &str) -> Option<String> {
    (given != canonical).then(|| key_alias_line(given, canonical))
}

/// Commands that dispatch before clap and keep their names.
const NEVER_REWRITTEN: &str = "hook";

/// Rewrites a leading old command path in `args` (which holds the program
/// name first) to its new path, and returns the deprecation line for it.
/// Leading arguments that start with `-` are skipped before matching.
pub fn rewrite(args: &[OsString], table: &[Rename]) -> (Vec<OsString>, Option<String>) {
    let words: Vec<Option<&str>> = args.iter().map(|a| a.to_str()).collect();
    let mut start = 1;
    while words
        .get(start)
        .is_some_and(|w| w.is_some_and(|w| w.starts_with('-')))
    {
        start += 1;
    }
    if words.get(start) == Some(&Some("help")) {
        start += 1;
    }
    if words.get(start) == Some(&Some(NEVER_REWRITTEN)) {
        return (args.to_vec(), None);
    }
    let matched = table
        .iter()
        .filter(|row| {
            let old: Vec<&str> = row.old.split(' ').collect();
            words.len() >= start + old.len()
                && old.iter().zip(&words[start..]).all(|(o, w)| *w == Some(*o))
        })
        .max_by_key(|row| row.old.split(' ').count());
    let Some(row) = matched else {
        return (args.to_vec(), None);
    };
    let old_len = row.old.split(' ').count();
    let mut out: Vec<OsString> = args[..start].to_vec();
    out.extend(row.new.split(' ').map(OsString::from));
    out.extend(row.extra.iter().map(OsString::from));
    out.extend_from_slice(&args[start + old_len..]);
    (out, Some(deprecation_line(row.old, row.new)))
}

/// The deprecation line for a leading command path in `args` that `table`
/// lists, or `None`.
pub fn notice_for(args: &[OsString], table: &[Rename]) -> Option<String> {
    rewrite(args, table).1
}

/// The line `main` prints, or `None` when `--json` is among `args` or
/// stderr is not a terminal.
pub fn printable_notice(
    args: &[OsString],
    line: Option<String>,
    stderr_is_terminal: bool,
) -> Option<String> {
    let json = args.iter().any(|a| a == JSON_FLAG);
    line.filter(|_| stderr_is_terminal && !json)
}

const JSON_FLAG: &str = "--json";

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &[Rename] = &[
        Rename {
            old: "pr ci-wait",
            new: "ci wait",
            extra: &[],
        },
        Rename {
            old: "pr land",
            new: "land",
            extra: &["--auto"],
        },
        Rename {
            old: "pr",
            new: "review pr",
            extra: &[],
        },
        Rename {
            old: "hook",
            new: "nothook",
            extra: &[],
        },
    ];

    fn argv(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn rewritten(words: &[&str]) -> (Vec<String>, bool) {
        let (out, line) = rewrite(&argv(words), TABLE);
        let out = out
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        (out, line.is_some())
    }

    #[test]
    fn an_old_prefix_is_replaced_and_the_rest_is_kept() {
        // Arrange
        let args = ["playbook", "pr", "ci-wait", "12", "--timeout", "5"];

        // Act
        let (out, noticed) = rewritten(&args);

        // Assert
        assert_eq!(out, ["playbook", "ci", "wait", "12", "--timeout", "5"]);
        assert!(noticed);
    }

    #[test]
    fn extra_words_go_after_the_new_words() {
        // Arrange
        let args = ["playbook", "pr", "land", "7"];

        // Act
        let (out, _) = rewritten(&args);

        // Assert
        assert_eq!(out, ["playbook", "land", "--auto", "7"]);
    }

    #[test]
    fn the_notice_names_the_old_and_new_path_and_the_removal_release() {
        // Arrange
        let args = argv(&["playbook", "pr", "ci-wait", "1"]);

        // Act
        let (_, line) = rewrite(&args, TABLE);

        // Assert
        let line = line.expect("a rewrite has a notice");
        assert_eq!(line, deprecation_line("pr ci-wait", "ci wait"));
        for part in ["pr ci-wait", "ci wait", ALIAS_REMOVAL] {
            assert!(line.contains(part), "{line}");
        }
    }

    #[test]
    fn a_word_that_only_starts_like_an_old_word_is_not_matched() {
        // Arrange
        let args = argv(&["playbook", "pr", "ci-waitx", "1"]);
        let table = &TABLE[..1];

        // Act
        let (out, line) = rewrite(&args, table);

        // Assert
        assert_eq!(out, args);
        assert!(line.is_none());
    }

    #[test]
    fn hook_is_never_touched() {
        // Arrange
        let args = argv(&["playbook", "hook", "pre-tool-use"]);

        // Act
        let (out, line) = rewrite(&args, TABLE);

        // Assert
        assert_eq!(out, args);
        assert!(line.is_none());
    }

    #[test]
    fn help_and_the_help_flag_are_rewritten_too() {
        // Arrange
        let rows: [(&[&str], &[&str]); 2] = [
            (
                &["playbook", "help", "pr", "ci-wait"],
                &["playbook", "help", "ci", "wait"],
            ),
            (
                &["playbook", "pr", "ci-wait", "--help"],
                &["playbook", "ci", "wait", "--help"],
            ),
        ];

        for (input, want) in rows {
            // Act
            let (out, noticed) = rewritten(input);

            // Assert
            assert_eq!(out, want, "for {input:?}");
            assert!(noticed, "for {input:?}");
        }
    }

    #[test]
    fn a_global_flag_before_the_subcommand_keeps_the_rewrite_working() {
        // Arrange
        let args = ["playbook", "--some-flag", "pr", "ci-wait", "3"];

        // Act
        let (out, noticed) = rewritten(&args);

        // Assert
        assert_eq!(out, ["playbook", "--some-flag", "ci", "wait", "3"]);
        assert!(noticed);
    }

    #[test]
    fn version_is_left_alone() {
        // Arrange
        let args = argv(&["playbook", "--version"]);

        // Act
        let (out, line) = rewrite(&args, TABLE);

        // Assert
        assert_eq!(out, args);
        assert!(line.is_none());
    }

    #[test]
    fn the_longest_matching_old_path_wins() {
        // Arrange
        let args = ["playbook", "pr", "ci-wait", "1"];

        // Act
        let (out, _) = rewritten(&args);

        // Assert
        assert_eq!(&out[1..3], ["ci", "wait"]);
    }

    #[test]
    fn json_or_a_pipe_suppresses_the_notice() {
        // Arrange
        let line = || Some("old is renamed".to_string());
        let plain = argv(&["playbook", "pr", "ci-wait", "1"]);
        let json = argv(&["playbook", "pr", "ci-wait", "1", "--json"]);
        let rows = [
            (&plain, true, true),
            (&plain, false, false),
            (&json, true, false),
            (&json, false, false),
        ];

        for (args, terminal, printed) in rows {
            // Act
            let got = printable_notice(args, line(), terminal);

            // Assert
            assert_eq!(got.is_some(), printed, "terminal {terminal}, args {args:?}");
        }
    }

    #[test]
    fn the_key_notice_names_both_keys_and_the_removal_release() {
        // Arrange
        let (old, new) = ("old.key", "new.key");

        // Act
        let line = key_alias_line(old, new);

        // Assert
        for part in [old, new, ALIAS_REMOVAL] {
            assert!(line.contains(part), "{line}");
        }
    }

    #[test]
    fn a_key_notice_exists_only_for_a_renamed_name() {
        // Arrange
        let renamed = ("old.key", "new.key");

        // Act
        let for_old = key_notice(renamed.0, renamed.1);
        let for_new = key_notice(renamed.1, renamed.1);

        // Assert
        assert_eq!(for_old, Some(key_alias_line(renamed.0, renamed.1)));
        assert_eq!(for_new, None);
    }
}
