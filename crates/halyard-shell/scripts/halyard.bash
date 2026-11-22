# Halyard shell integration for bash (3.2 and later).
#
# Emits OSC 133 prompt marks (A, B, C, D with the exit status), OSC 633;E with the
# command line, and OSC 7 with the working directory, so Halyard can build command
# blocks. Source it from ~/.bashrc. It is safe to source more than once, and it does
# nothing unless TERM_PROGRAM is Halyard and the shell is interactive.
#
# Existing hooks are kept: PROMPT_COMMAND gets one function added before its
# current contents (to read the exit status first) and two entries after; an
# existing DEBUG trap keeps running after this script's own. A DEBUG trap set later
# by something else replaces this one, and then only the C mark is lost.
#
# SPDX-License-Identifier: MIT OR Apache-2.0

[[ ${TERM_PROGRAM-} == Halyard && $- == *i* ]] || return 0
[[ -n ${__halyard_bash_loaded-} ]] && return 0
__halyard_bash_loaded=1
__halyard_running=0
__halyard_at_prompt=0
__halyard_status=0
__halyard_hist=
__halyard_line=

__halyard_cwd() {
  local LC_ALL=C s=$PWD out='' c i
  for (( i = 0; i < ${#s}; i++ )); do
    c=${s:i:1}
    case $c in
      [A-Za-z0-9/._~-]) out+=$c ;;
      *)
        # bash 3.2 sign-extends bytes above 0x7f, hence the mask.
        printf -v c '%d' "'$c"
        printf -v c '%%%02X' $(( c & 255 ))
        out+=$c
        ;;
    esac
  done
  printf '\033]7;file://%s%s\007' "${HOSTNAME-}" "$out"
}

# bash saves and restores `$_` around PROMPT_COMMAND itself, so only the DEBUG trap
# below has to take care of it.
__halyard_prompt_start() {
  __halyard_status=$?
  __halyard_at_prompt=0
  return "$__halyard_status"
}

__halyard_prompt_end() {
  local ret=$__halyard_status
  if [[ $__halyard_running == 1 ]]; then
    printf '\033]133;D;%d;aid=%d\007' "$ret" "$$"
    __halyard_running=0
  fi
  __halyard_cwd
  printf '\033]133;A;aid=%d\007' "$$"
  case $PS1 in
    *'\[\033]133;B\007\]'*) ;;
    *) PS1=$PS1'\[\033]133;B\007\]' ;;
  esac
  __halyard_hist=$(HISTTIMEFORMAT='' builtin history 1)
  __halyard_keep_first
  __halyard_at_prompt=1
  return "$ret"
}

# Keeps __halyard_prompt_start first in PROMPT_COMMAND. It clears the "at the
# prompt" flag, so when it runs first nothing else in PROMPT_COMMAND can be taken
# for the user's command by the DEBUG trap, whatever its text. Commands that other
# tools put in front of it after this script was sourced are moved behind it here,
# and take effect from the next prompt.
__halyard_keep_first() {
  if [[ $__halyard_pc_array == 1 ]]; then
    [[ ${PROMPT_COMMAND[0]} == "$__halyard_start_cmd" ]] && return 0
    local -a rest=()
    local el
    for el in "${PROMPT_COMMAND[@]}"; do
      el=${el//"$__halyard_start_cmd"/}
      [[ -n ${el//[[:space:];]/} ]] && rest+=("$el")
    done
    PROMPT_COMMAND=("$__halyard_start_cmd" "${rest[@]}")
  else
    # shellcheck disable=SC2128 # a scalar here
    case $PROMPT_COMMAND in
      "$__halyard_start_cmd"$'\n'*) return 0 ;;
    esac
    # shellcheck disable=SC2178 # a scalar PROMPT_COMMAND stays a scalar
    PROMPT_COMMAND=$__halyard_start_cmd$'\n'${PROMPT_COMMAND//"$__halyard_start_cmd"/}
  fi
}

# True when $1 is exactly one of the commands in PROMPT_COMMAND (split on newlines
# and semicolons, surrounding blanks ignored). The same test bash-preexec uses.
__halyard_in_prompt_command() {
  local IFS=$'\n;' piece
  local -a pieces
  read -r -d '' -a pieces <<< "${PROMPT_COMMAND[*]}" || :
  for piece in "${pieces[@]}"; do
    piece=${piece#"${piece%%[![:space:]]*}"}
    piece=${piece%"${piece##*[![:space:]]}"}
    [[ $piece == "$1" ]] && return 0
  done
  return 1
}

# Strips the `  123* ` prefix that `history 1` puts before a command line.
__halyard_strip_history_number() {
  local l=$1
  l=${l#"${l%%[![:space:]]*}"}
  l=${l#"${l%%[!0-9]*}"}
  l=${l#\*}
  l=${l#"${l%%[![:space:]]*}"}
  __halyard_line=$l
}

# The DEBUG trap calls this as `__halyard_preexec "$_"`: ending the trap with `$_`
# as the last argument keeps `$_` intact for the user's command.
__halyard_preexec() {
  [[ $__halyard_at_prompt == 1 ]] || return 0
  [[ -n ${COMP_LINE-} ]] && return 0
  local line
  line=$(HISTTIMEFORMAT='' builtin history 1)
  if [[ $line == "$__halyard_hist" ]]; then
    # History did not move: either PROMPT_COMMAND is starting after an empty line,
    # or history did not record this command (ignorespace, ignoredups, set +o
    # history). __halyard_keep_first makes __halyard_prompt_start the first
    # command of PROMPT_COMMAND, and it clears the flag; a command that is
    # literally part of PROMPT_COMMAND is also skipped, for tools that keep
    # putting themselves first.
    [[ $BASH_COMMAND == "$__halyard_start_cmd" ]] && return 0
    __halyard_in_prompt_command "$BASH_COMMAND" && return 0
    line=
  else
    __halyard_strip_history_number "$line"
    line=$__halyard_line
  fi
  __halyard_at_prompt=0
  if [[ -n $line ]]; then
    # OSC 633 escaping: backslash, semicolon and newline become \\, \x3b, \x0a.
    line=${line//\\/\\\\}
    line=${line//;/\\x3b}
    line=${line//$'\n'/\\x0a}
    line=${line//[[:cntrl:]]/}
    printf '\033]633;E;%s\007' "$line"
  fi
  printf '\033]133;C\007'
  __halyard_running=1
  return 0
}

__halyard_trap_arg() { __halyard_old_debug=${3-}; }

# Builds the DEBUG trap: ours first, then whatever trap was already set.
# shellcheck disable=SC2016 # `$_` is expanded when the trap runs
__halyard_make_trap() {
  if [[ -n ${__halyard_old_debug-} ]]; then
    __halyard_trap='__halyard_preexec "$_"'$'\n'"$__halyard_old_debug"
  else
    __halyard_trap='__halyard_preexec "$_"'
  fi
}

# bash restores the DEBUG trap when `source` returns, so the trap cannot be set
# here. This PROMPT_COMMAND entry sets it once, at top level, on the first prompt.
__halyard_setup=1
# shellcheck disable=SC2016 # expanded later, when PROMPT_COMMAND runs
__halyard_setup_cmd='if [[ -n ${__halyard_setup-} ]]; then eval "__halyard_trap_arg $(trap -p DEBUG)"; __halyard_make_trap; trap -- "$__halyard_trap" DEBUG; __halyard_setup=; fi'
__halyard_start_cmd=__halyard_prompt_start
__halyard_end_cmd=__halyard_prompt_end

__halyard_pc_array=0
if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
  __halyard_pc_array=1
  PROMPT_COMMAND=("$__halyard_start_cmd" "${PROMPT_COMMAND[@]}" "$__halyard_setup_cmd" "$__halyard_end_cmd")
else
  # shellcheck disable=SC2178 # a scalar PROMPT_COMMAND stays a scalar
  PROMPT_COMMAND="$__halyard_start_cmd"$'\n'"${PROMPT_COMMAND-}"$'\n'"$__halyard_setup_cmd"$'\n'"$__halyard_end_cmd"
fi
