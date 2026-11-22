# Halyard shell integration for fish.
#
# Emits OSC 7 with the working directory on every prompt. fish 4 already marks
# prompts and commands itself with OSC 133 (A, B, C with `cmdline_url`, D with the
# exit status), so on fish 4 and later this script adds only OSC 7. On fish 3 it
# also emits the OSC 133 marks and OSC 633;E with the command line.
#
# Source it from ~/.config/fish/config.fish. It is safe to source more than once,
# and it does nothing unless TERM_PROGRAM is Halyard and the shell is interactive.
# It adds event handlers; on fish 3 it also wraps fish_prompt to append the B mark,
# and the user's prompt function still draws the prompt.
#
# SPDX-License-Identifier: MIT OR Apache-2.0

if test "$TERM_PROGRAM" = Halyard; and status is-interactive; and not set -q __halyard_fish_loaded
    set -g __halyard_fish_loaded 1

    function __halyard_cwd --on-event fish_prompt
        printf '\e]7;file://%s%s\a' $hostname (string escape --style=url -- $PWD)
    end

    if string match -qr '^[0-3]\.' -- $version
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

        if functions -q fish_prompt; and not functions -q __halyard_user_fish_prompt
            functions -c fish_prompt __halyard_user_fish_prompt
            function fish_prompt
                __halyard_user_fish_prompt
                printf '\e]133;B\a'
            end
        end
    end
end
