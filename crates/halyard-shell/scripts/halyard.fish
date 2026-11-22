# Halyard shell integration for fish.
#
# Emits OSC 7 with the working directory on every prompt. fish 4 already marks
# prompts and commands itself with OSC 133 (A, B, C with `cmdline_url`, D with the
# exit status), so when fish's own marking is on this script adds only OSC 7.
# Otherwise (fish 3, or fish 4 with `set -Ua fish_features no-mark-prompt`) it also
# emits the OSC 133 marks and OSC 633;E with the command line.
#
# fish 4.0 to 4.2 send no B mark, so there the script wraps fish_prompt to append it.
# Known gaps of fish's own marks, not filled here: fish 4.0.0 sends no command text,
# and fish 4.8 and later turn marking off when they
# believe they run in Konsole (`status test-feature` may not reflect that). Its marks
# carry no `aid=`; Halyard's block model treats an aid-less shell as its own level.
#
# Source it from ~/.config/fish/config.fish. It is safe to source more than once,
# and it does nothing unless TERM_PROGRAM is Halyard and the shell is interactive.
# It adds event handlers; when it emits marks it also wraps fish_prompt to append the
# B mark, and the user's prompt function still draws the prompt.
#
# SPDX-License-Identifier: MIT OR Apache-2.0

if test "$TERM_PROGRAM" = Halyard; and status is-interactive; and not set -q __halyard_fish_loaded
    set -g __halyard_fish_loaded 1

    function __halyard_cwd --on-event fish_prompt
        printf '\e]7;file://%s%s\a' $hostname (string escape --style=url -- $PWD)
    end

    # Does fish mark prompts itself? fish 3: no. fish 4: yes unless the mark-prompt
    # feature is off. `status test-feature` returns 0 (on), 1 (off), or 2 (no such
    # feature: fish 4.0.0 to 4.0.5, which always mark).
    set -l __halyard_native_marks 0
    if not string match -qr '^[0-3]\.' -- $version
        status test-feature mark-prompt 2>/dev/null
        if test $status -ne 1
            set __halyard_native_marks 1
        end
    end

    # fish 4.0 to 4.2 mark prompts natively but send no B mark (fish 4.3 added it), and the
    # blocks need B to know where the command line starts. Append it to the prompt there.
    set -l __halyard_needs_b 0
    if test $__halyard_native_marks = 0
        set __halyard_needs_b 1
    else if string match -qr '^4\.[0-2](\.|-|$)' -- $version
        set __halyard_needs_b 1
    end

    if test $__halyard_native_marks = 0
        function __halyard_prompt_mark --on-event fish_prompt
            printf '\e]133;A;aid=%d\a' $fish_pid
        end

        function __halyard_preexec --on-event fish_preexec
            # OSC 633 escaping: backslash, semicolon and newline become \\, \x3b, \x0a.
            set -l cmd (string join \n -- $argv[1] | string replace -a '\\' '\\\\' | string replace -a ';' '\\x3b' | string join '\\x0a' | string replace -ra '[[:cntrl:]]' '')
            printf '\e]633;E;%s\a\e]133;C\a' "$cmd"
        end

        function __halyard_postexec --on-event fish_postexec
            set -l s $status
            printf '\e]133;D;%d;aid=%d\a' $s $fish_pid
        end
    end

    if test $__halyard_needs_b = 1
        if functions -q fish_prompt; and not functions -q __halyard_user_fish_prompt
            functions -c fish_prompt __halyard_user_fish_prompt
            function fish_prompt
                __halyard_user_fish_prompt
                printf '\e]133;B\a'
            end
        end
    end
end
