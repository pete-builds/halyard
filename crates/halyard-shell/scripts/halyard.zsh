# Halyard shell integration for zsh.
#
# Emits OSC 133 prompt marks (A, B, C, D with the exit status), OSC 633;E with the
# command line, and OSC 7 with the working directory, so Halyard can build command
# blocks. Source it from ~/.zshrc. It is safe to source more than once, and it does
# nothing unless TERM_PROGRAM is Halyard and the shell is interactive. Existing
# precmd and preexec hooks are kept: this only appends its own.
#
# SPDX-License-Identifier: MIT OR Apache-2.0

[[ ${TERM_PROGRAM-} == Halyard && -o interactive ]] || return 0
(( ${+__halyard_zsh_loaded} )) && return 0
typeset -g __halyard_zsh_loaded=1
typeset -gi __halyard_running=0

__halyard_cwd() {
  emulate -L zsh
  local LC_ALL=C s=$PWD out= c
  local -i i
  for (( i = 1; i <= ${#s}; i++ )); do
    c=${s[i]}
    case $c in
      [A-Za-z0-9/._~-]) out+=$c ;;
      *) printf -v c '%%%02X' "'$c"; out+=$c ;;
    esac
  done
  printf '\e]7;file://%s%s\a' "${HOST-}" "$out"
}

__halyard_precmd() {
  # zsh gives every precmd hook the command's status, whatever earlier hooks did.
  # Read it before anything else runs.
  local -i ret=$?
  emulate -L zsh
  if (( __halyard_running )); then
    printf '\e]133;D;%d;aid=%d\a' $ret $$
    __halyard_running=0
  fi
  __halyard_cwd
  printf '\e]133;A;aid=%d\a' $$
  # Mark the end of the prompt. A theme that rebuilds PS1 later in its own hook can
  # drop this; Halyard tolerates a missing B.
  local mark=$'%{\e]133;B\a%}'
  [[ $PS1 == *$mark* ]] || PS1=$PS1$mark
}

__halyard_preexec() {
  emulate -L zsh
  local cmd=$1
  # OSC 633 escaping: backslash, semicolon and newline are written as \\, \x3b, \x0a.
  cmd=${cmd//\\/\\\\}
  cmd=${cmd//;/\\x3b}
  cmd=${cmd//$'\n'/\\x0a}
  cmd=${cmd//[[:cntrl:]]/}
  printf '\e]633;E;%s\a\e]133;C\a' "$cmd"
  __halyard_running=1
}

precmd_functions+=(__halyard_precmd)
preexec_functions+=(__halyard_preexec)
