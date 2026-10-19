# ezlo zsh integration — sourced via `eval "$(ezlo init zsh)"`
[[ -o interactive ]] || return 0

zmodload zsh/net/socket 2>/dev/null || return 0
# sysread: the `read` builtin pulls sockets one byte per syscall, too slow for every keystroke.
zmodload zsh/system 2>/dev/null || return 0
autoload -Uz add-zsh-hook add-zle-hook-widget

typeset -g _ezlo_bin="${EZLO_BIN:-ezlo}"
typeset -g _ezlo_sock="${EZLO_SOCKET:-${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}/ezlo-$UID}/ezlo.sock}"
typeset -gi _ezlo_fd=-1
typeset -gi _ezlo_req_id=0
typeset -gi _ezlo_resp_id=0
typeset -gi _ezlo_start=0
typeset -gi _ezlo_end=0
# Selected dropdown row; 0: none (Enter runs the typed line). Up/Down select, Enter accepts.
typeset -gi _ezlo_selected=0
typeset -gi _ezlo_menu_visible=0
typeset -gi _ezlo_server_rows=0
typeset -gi _ezlo_spawned_daemon=0
typeset -gi _ezlo_enabled=1
typeset -gi _ezlo_pending=0
typeset -gi _ezlo_drawn=0
typeset -g _ezlo_ghost=""
typeset -g _ezlo_last_buf=""
typeset -gi _ezlo_last_cur=-1
typeset -g _ezlo_req_buf=""
# Daemon output read but not consumed yet: complete lines (_ezlo_rlines from _ezlo_rpos on) and a
# trailing partial line (_ezlo_rbuf). See _ezlo_getline.
typeset -g _ezlo_rbuf=""
typeset -ga _ezlo_rlines=()
typeset -gi _ezlo_rpos=1
# Set while BUFFER is an unedited line recalled via Up/Down: ezlo stays quiet so repeated Up/Down
# keep walking history instead of navigating a dropdown that popped up for the recalled line.
typeset -gi _ezlo_hist_nav=0
typeset -g _ezlo_hist_buf=""
typeset -ga _ezlo_disp=()
typeset -ga _ezlo_hl=()
typeset -ga _ezlo_ins=()
typeset -ga _ezlo_suf=()
typeset -ga _ezlo_kind=()
typeset -ga _ezlo_desc=()
# Full text of truncated rows (`W` field 9; empty when the row fits).
typeset -ga _ezlo_detail=()
typeset -ga _ezlo_diag_span=()
typeset -ga _ezlo_diag_sev=()
typeset -ga _ezlo_diag_msg=()
# Blast-radius / dry-run effect preview lines (`P` lines of a response) and their kinds.
typeset -ga _ezlo_prev_text=()
typeset -ga _ezlo_prev_kind=()
# Effect lines kept below an open dropdown (`zstyle ':ezlo:preview' max-lines`, default 2).
typeset -gi _ezlo_prev_max=2
zstyle -s ':ezlo:preview' max-lines _ezlo_prev_max || _ezlo_prev_max=2
# Positional signature hint (`R` header fields 11-13): text and active placeholder char range.
typeset -g _ezlo_sig=""
typeset -g _ezlo_sig_active=""
# Candidate preview pane (`[complete.pane]`): per-row availability (`W` field 8: auto | manual |
# empty), the shown lines + styles (`V` reply to `PANE`), the "<resp id>:<row>" they belong to and
# the last one requested. _ezlo_pane_force: 0 follows the daemon's `auto`, 1 on, -1 off (Alt-E menu).
# _ezlo_pane_col (`R` header field 14): column where the pane starts right of the rows (the daemon
# laid them out up to it); 0 draws it below the dropdown.
typeset -ga _ezlo_pane_avail=()
typeset -ga _ezlo_pane_lines=()
typeset -ga _ezlo_pane_styles=()
# Colored runs of each pane line (`from-to:style …`, chars of the line; from bat/eza output).
typeset -ga _ezlo_pane_spans=()
typeset -g _ezlo_pane_key=""
typeset -g _ezlo_pane_req=""
typeset -gi _ezlo_pane_force=0
typeset -gi _ezlo_pane_col=0
# _ezlo_pane_height (`V` header field 5, `[complete.pane] lines`): visible pane lines (beside the
# rows, at least the dropdown's height). The daemon sends the whole preview: _ezlo_pane_off is the
# number of lines scrolled past (Shift-Up/Down), _ezlo_pane_view the preview lines last shown.
typeset -gi _ezlo_pane_height=5
typeset -gi _ezlo_pane_off=0
typeset -gi _ezlo_pane_view=0
# Alt-H explanation overlay: shown until the buffer/cursor changes or Esc.
typeset -ga _ezlo_explain_lines=()
typeset -ga _ezlo_explain_styles=()
typeset -gi _ezlo_explain_id=0
typeset -g _ezlo_explain_buf=""
typeset -gi _ezlo_explain_cur=-1
# Comment-line translation (`[complete.translate]`: Enter on the "translate" row, or Ctrl-X Ctrl-L): id of
# the running `TRANSLATE`, the line it translates, state (busy | err | empty: none) and error message.
# Its suggestions arrive as an `R` response of `translated` rows and are shown as the dropdown.
typeset -gi _ezlo_tr_id=0
typeset -g _ezlo_tr_buf=""
typeset -g _ezlo_tr_state=""
typeset -g _ezlo_tr_msg=""
# Alt-E settings popup (session-scoped, never persisted): open flag and selected row; _ezlo_set holds
# this shell's overrides (daemon keys go out with `SET`, `ghost` stays here; a value equal to its
# default is dropped), _ezlo_cfg the daemon's config values from the `S` reply. Suggestions on/off is
# _ezlo_enabled and the preview pane mode is _ezlo_pane_force.
typeset -gi _ezlo_popup_open=0
typeset -gi _ezlo_popup_sel=1
typeset -gA _ezlo_set=()
typeset -gA _ezlo_cfg=()
typeset -ga _ezlo_popup_keys=( enabled rows history_rows min_chars ghost signature diagnostics preview pane reset )
typeset -gA _ezlo_popup_labels=(
  enabled 'Suggestions' rows 'Dropdown rows' history_rows 'History rows' min_chars 'Min chars'
  ghost 'Ghost text' signature 'Signature hint' diagnostics 'Diagnostics' preview 'Effect preview'
  pane 'Preview pane' reset 'Reset to defaults'
)

# Tab: zsh's own completion, shown in ezlo's dropdown. zsh computes the matches and inserts the
# unambiguous prefix (or the only match) exactly as it would without ezlo; only its list is replaced
# by dropdown rows (laid out by the daemon, `NATIVE`). Tab/Shift-Tab or Up/Down pick a row and Enter
# inserts it by replaying zsh's own `compadd` call for that match (prefixes, suffixes, quoting), the
# way fzf-tab does. Per captured match: the word, its display string, the compadd options to replay
# (\1-joined), IPREFIX/ISUFFIX, the directory of file matches and the row kind.
zmodload zsh/zutil 2>/dev/null
typeset -gi _ezlo_native=0
typeset -gi _ezlo_cm_on=0
typeset -gi _ezlo_cm_show=0
typeset -gi _ezlo_cm_pick=0
typeset -ga _ezlo_cm_words=() _ezlo_cm_dscr=() _ezlo_cm_args=() _ezlo_cm_ipre=() _ezlo_cm_isuf=() _ezlo_cm_dir=() _ezlo_cm_kind=() _ezlo_cm_quote=()
# Capture index of each dropdown row (empty and duplicate words dropped, at most 500).
typeset -ga _ezlo_cm_rows=()

# Leading `~` / `~user` / named dir of $1 expanded into REPLY (nothing else is evaluated).
_ezlo_expand_tilde() {
  REPLY=$1
  [[ $1 == \~* ]] || return 0
  local head=${1%%/*} exp
  [[ $head == *[^[:alnum:]_.+~-]* ]] && return 0
  eval "exp=$head" 2>/dev/null || return 0
  REPLY=$exp${1:${#head}}
}

# Stands in for `compadd` while Tab completes: records the matches of each call, then adds them as
# usual so zsh's own insertion logic runs. Calls that only query (-O/-A/-D) pass through untouched.
_ezlo_compadd() {
  local -A apre hpre dscrs _oad
  local -a isfile _opts __ expl
  zparseopts -a _opts P:=apre p:=hpre d:=dscrs X+:=expl O:=_oad A:=_oad D:=_oad f=isfile \
    i: S: s: I: x:=__ r: R: W: F: M+: E: q e Q n U C \
    J:=__ V:=__ a=__ l=__ k=__ o::=__ 1=__ 2=__
  if (( $#_oad || ! _ezlo_cm_on || $#_ezlo_cm_words >= 2000 )); then
    builtin compadd "$@"
    return
  fi
  local -a __hits __dscr
  (( $#dscrs == 1 )) && __dscr=( "${(@P)${(v)dscrs}}" )
  builtin compadd -A __hits -D __dscr "$@"
  if (( $#__hits )); then
    _opts+=( "${(@kv)apre}" "${(@kv)hpre}" $isfile )
    local args=${(pj:\1:)_opts} dir="" kind=arg
    if [[ -n $isfile ]]; then
      kind=file
      _ezlo_expand_tilde "${(Q)IPREFIX}${(Q)${(v)hpre}}"
      dir=$REPLY
    elif [[ $curcontext == *:-command-:* ]]; then
      kind=cmd
    fi
    local -i i
    for (( i = 1; i <= $#__hits; i++ )); do
      _ezlo_cm_words+=( "$__hits[i]" )
      _ezlo_cm_dscr+=( "${__dscr[i]-}" )
      _ezlo_cm_args+=( "$args" )
      _ezlo_cm_ipre+=( "$IPREFIX" )
      _ezlo_cm_isuf+=( "$ISUFFIX" )
      _ezlo_cm_dir+=( "$dir" )
      _ezlo_cm_kind+=( "$kind" )
      _ezlo_cm_quote+=( "$compstate[quote]" )
    done
  fi
  builtin compadd "$@"
}

# Completion function of the Tab widget: zsh's `_main_complete` with matches recorded; when several
# match, zsh still inserts the unambiguous prefix but lists nothing (ezlo shows them instead).
_ezlo_complete_fn() {
  _ezlo_cm_words=() _ezlo_cm_dscr=() _ezlo_cm_args=() _ezlo_cm_ipre=() _ezlo_cm_isuf=() _ezlo_cm_dir=() _ezlo_cm_kind=() _ezlo_cm_quote=()
  _ezlo_cm_rows=()
  _ezlo_cm_show=0
  local saved=${functions[compadd]-}
  _ezlo_cm_on=1
  functions[compadd]=$functions[_ezlo_compadd]
  {
    _main_complete "$@"
  } always {
    _ezlo_cm_on=0
    if [[ -n $saved ]]; then functions[compadd]=$saved; else unfunction compadd 2>/dev/null; fi
  }
  (( compstate[nmatches] > 1 )) || return 0
  local -A seen
  local -i i
  for (( i = 1; i <= $#_ezlo_cm_words && $#_ezlo_cm_rows < 500; i++ )); do
    # `x` prefix: a bare `*` or `@` key would subscript the whole hash.
    [[ -z $_ezlo_cm_words[i] || -n ${seen[x${_ezlo_cm_words[i]}]-} ]] && continue
    seen[x${_ezlo_cm_words[i]}]=1
    _ezlo_cm_rows+=( $i )
  done
  # Matches zsh added behind our back can't be shown: let it list them itself.
  (( $#_ezlo_cm_rows > 1 )) || return 0
  [[ $compstate[insert] == *menu* ]] && compstate[insert]=unambiguous
  compstate[list]=''
  _ezlo_cm_show=1
}

# Inserts captured match _ezlo_cm_pick by replaying its compadd call (-U: replaces the typed word,
# which by now holds the unambiguous prefix zsh inserted on Tab).
_ezlo_native_apply_fn() {
  local -i i=$_ezlo_cm_pick
  (( i > 0 && i <= $#_ezlo_cm_words )) || return 1
  local -a args=( "${(@ps:\1:)_ezlo_cm_args[i]}" )
  [[ -z ${args[1]-} ]] && args=()
  local ipre=$_ezlo_cm_ipre[i] isuf=$_ezlo_cm_isuf[i]
  if [[ -n $ipre && $PREFIX == "$ipre"* ]]; then
    IPREFIX=$ipre
    PREFIX=${PREFIX:${#ipre}}
  fi
  if [[ -n $isuf && $SUFFIX == *"$isuf" ]]; then
    ISUFFIX=$isuf
    SUFFIX=${SUFFIX:0:$(( ${#SUFFIX} - ${#isuf} ))}
  fi
  # Captured outside quotes (escaped), inserted inside the `"` _ezlo_requote_word opened: requote
  # for it (-Q inserts verbatim).
  local word=$_ezlo_cm_words[i]
  if [[ -z $_ezlo_cm_quote[i] && $compstate[quote] == \" ]]; then
    setopt localoptions extendedglob
    word=${(Q)word}
    word=${word//(#m)[\\\"\$\`\!]/\\$MATCH}
  fi
  builtin compadd "${args[@]:--Q}" -U -Q -- "$word"
  compstate[insert]=1
  compstate[list]=''
}

# Builds the `NATIVE` message for the captured rows into REPLY.
_ezlo_native_msg() {
  local us=$'\x1f' bad=$'[\x1f\n\r]'
  local msg="NATIVE${us}${_ezlo_req_id}" w label d desc kind mpath
  local -i i
  for i in "${_ezlo_cm_rows[@]}"; do
    w=$_ezlo_cm_words[i]
    label=${(Q)w}
    d=${_ezlo_cm_dscr[i]//$'\n'/ }
    desc=""
    if [[ $d == *' -- '* ]]; then
      desc=${d#* -- }
    elif [[ -n $d && $d != "$label" && $d != "$w" ]]; then
      desc=$d
    fi
    kind=$_ezlo_cm_kind[i]
    [[ $kind == arg && $label == -* ]] && kind=opt
    mpath=""
    if [[ $kind == file ]]; then
      mpath=$_ezlo_cm_dir[i]$label
      [[ $mpath == /* ]] || mpath=$PWD/$mpath
    fi
    msg+="${us}${label//${~bad}/ }${us}${desc//${~bad}/ }${us}${kind}${us}${mpath//${~bad}/}"
  done
  REPLY=$msg
}

# The builtin completion widget Tab used before ezlo (menu variants without the menu): what the
# Tab widget completes with. Wrappers that highlighters/autosuggestions put around widgets
# (zsh-syntax-highlighting & co: `_zsh_highlight_widget_<orig>`, zsh-autosuggestions:
# `_zsh_autosuggest_bound_<n>_<w>` → `autosuggest-orig-<n>-<w>`) are followed to what they wrap.
# Fails when Tab ends at any other user widget (e.g. fzf-completion): Tab is left alone then.
_ezlo_native_base() {
  local w=_ezlo_orig_tab def
  local -i hops
  REPLY=""
  for (( hops = 0; hops < 8; hops++ )); do
    def=${widgets[$w]-}
    case $def in
      completion:*:_main_complete)
        REPLY=${${def#completion:}%%:*}
        break
        ;;
      builtin)
        # A copy (`zle -A`) doesn't tell which builtin it is; Tab's default is expand-or-complete.
        [[ $w == _ezlo_orig_tab ]] && REPLY=.expand-or-complete || REPLY=.${w#.}
        break
        ;;
      user:_zsh_highlight_widget_*) w=${def#user:_zsh_highlight_widget_} ;;
      user:_zsh_autosuggest_bound_[0-9]*_*)
        w=${def#user:_zsh_autosuggest_bound_}
        w=autosuggest-orig-${w/_/-}
        ;;
      *) return 1 ;;
    esac
  done
  case $REPLY in
    .menu-complete|.reverse-menu-complete) REPLY=.complete-word ;;
    .menu-expand-or-complete) REPLY=.expand-or-complete ;;
    .complete-word|.expand-or-complete|.expand-or-complete-prefix) ;;
    *) return 1 ;;
  esac
}

# Rewrites the word zsh just completed with backslash escapes (`Harry\ Styles/Adore\ You.jpg `) into
# double quotes (`"Harry Styles/Adore You.jpg" `), easier to read. $1: LBUFFER before completing. A
# leading `~user/` or `$VAR/` stays outside the quotes, a partial match (no suffix space) leaves the
# quote open so the next Tab keeps completing inside it, as zsh does when the word starts with `"`.
# Words with other quoting, `!` (history expansion), `=` or a leading `-` are left as zsh wrote them.
_ezlo_requote_word() {
  setopt localoptions extendedglob
  local olbuf=$1 w="" head="" close=""
  local -a words=( ${(z)olbuf} )
  (( $#words )) && [[ $olbuf == *"$words[-1]" ]] && w=$words[-1]
  local -i wstart=$(( ${#olbuf} - ${#w} ))
  [[ ${LBUFFER:0:$wstart} == "${olbuf:0:$wstart}" ]] || return 0
  w=${LBUFFER:$wstart}
  [[ $w == *\\* && $w != -* && $w != *=* ]] || return 0
  if [[ $w == *[^\\]' ' ]]; then
    w=${w% }
    close='" '
  fi
  if [[ $w == (\~|\$)[[:alnum:]_{}]#/* ]]; then
    head=${w%%/*}/
    w=${w#*/}
  fi
  [[ $w == ([[:alnum:]_./+,:@%~-]|\\?)## ]] || return 0
  local u=${(Q)w}
  [[ $u == *\!* ]] && return 0
  LBUFFER=${LBUFFER:0:$wstart}$head\"${u//(#m)[\\\"\$\`]/\\$MATCH}$close
}

_ezlo_tab() {
  if (( _ezlo_native && _ezlo_menu_visible && ${#_ezlo_disp} )); then
    _ezlo_down
    return
  fi
  if (( ! _ezlo_enabled || ! $+functions[_main_complete] )) || ! _ezlo_native_base; then
    zle _ezlo_orig_tab
    return
  fi
  local base=$REPLY
  # No daemon: zsh's own list rather than none.
  if ! _ezlo_connect; then
    zle _ezlo_orig_tab
    return
  fi
  zle -C _ezlo_complete "$base" _ezlo_complete_fn
  _ezlo_clear_state
  local olbuf=$LBUFFER
  zle _ezlo_complete
  _ezlo_requote_word "$olbuf"
  (( _ezlo_cm_show )) || return 0
  _ezlo_cm_show=0
  (( _ezlo_req_id++ ))
  _ezlo_req_buf=$BUFFER
  _ezlo_last_buf=$BUFFER
  _ezlo_last_cur=$CURSOR
  _ezlo_pending=1
  _ezlo_native=1
  _ezlo_native_msg
  _ezlo_send_raw "$REPLY" || { _ezlo_native=0; return; }
  _ezlo_sync
  if (( ! _ezlo_menu_visible )); then
    # No rows came back (e.g. a daemon older than this script): zsh's own list then.
    _ezlo_native=0
    zle list-choices
    return
  fi
  _ezlo_render
  zle -R
}

# Shift-Tab: previous row of a Tab list; its previous binding otherwise.
_ezlo_stab() {
  if (( _ezlo_native && _ezlo_menu_visible && ${#_ezlo_disp} )); then
    _ezlo_up
  else
    zle _ezlo_orig_stab
  fi
}

# Enter on a selected Tab row.
_ezlo_native_accept() {
  _ezlo_cm_pick=${_ezlo_cm_rows[_ezlo_selected]-0}
  _ezlo_clear_state
  if (( _ezlo_cm_pick )); then
    local olbuf=$LBUFFER
    zle _ezlo_native_apply
    _ezlo_requote_word "$olbuf"
  fi
  _ezlo_cm_pick=0
  _ezlo_on_redraw
  zle -R
}

_ezlo_send_raw() {
  (( _ezlo_fd >= 0 )) || return 1
  print -r -u $_ezlo_fd -- "$1" 2>/dev/null || {
    _ezlo_disconnect
    return 1
  }
}

# Next line from the daemon into REPLY. Chunks are split once into the _ezlo_rlines queue (per-line
# pattern ops on the whole buffer are quadratic in zsh); _ezlo_rbuf keeps the trailing partial line.
# $1: timeout in seconds (unset: block, 0: only what is already there). Status: 0 ok, 4 timeout,
# else EOF/error.
_ezlo_getline() {
  local chunk
  local -i rc
  while (( _ezlo_rpos > ${#_ezlo_rlines} )); do
    (( _ezlo_fd >= 0 )) || return 2
    if [[ -n ${1-} ]]; then
      sysread -t $1 -i $_ezlo_fd -s 65536 chunk
    else
      sysread -i $_ezlo_fd -s 65536 chunk
    fi
    rc=$?
    (( rc == 0 )) || return $rc
    [[ $chunk == *$'\n'* ]] || { _ezlo_rbuf+=$chunk; continue; }
    chunk=$_ezlo_rbuf$chunk
    _ezlo_rlines=( "${(@ps:\n:)chunk}" )
    _ezlo_rbuf=${_ezlo_rlines[-1]}
    _ezlo_rlines[-1]=()
    _ezlo_rpos=1
  done
  REPLY=${_ezlo_rlines[_ezlo_rpos]}
  (( _ezlo_rpos++ ))
}

_ezlo_disconnect() {
  if (( _ezlo_fd >= 0 )); then
    zle -F $_ezlo_fd 2>/dev/null
    exec {_ezlo_fd}>&- 2>/dev/null
    _ezlo_fd=-1
  fi
  _ezlo_rbuf=""
  _ezlo_rlines=()
  _ezlo_rpos=1
  _ezlo_cfg=()
  _ezlo_clear_state
}

_ezlo_clear_state() {
  _ezlo_pending=0
  _ezlo_drawn=0
  _ezlo_menu_visible=0
  _ezlo_selected=0
  _ezlo_native=0
  _ezlo_ghost=""
  _ezlo_disp=()
  _ezlo_hl=()
  _ezlo_ins=()
  _ezlo_suf=()
  _ezlo_kind=()
  _ezlo_desc=()
  _ezlo_detail=()
  _ezlo_diag_span=()
  _ezlo_diag_sev=()
  _ezlo_diag_msg=()
  _ezlo_prev_text=()
  _ezlo_prev_kind=()
  _ezlo_sig=""
  _ezlo_sig_active=""
  _ezlo_pane_avail=()
  _ezlo_pane_lines=()
  _ezlo_pane_styles=()
  _ezlo_pane_spans=()
  _ezlo_pane_key=""
  _ezlo_pane_req=""
  _ezlo_pane_off=0
  _ezlo_pane_col=0
  _ezlo_explain_lines=()
  _ezlo_explain_styles=()
  _ezlo_translate_cancel
  if zle 2>/dev/null; then
    POSTDISPLAY=""
    region_highlight=( ${region_highlight:#*memo=ezlo} )
    (( _ezlo_popup_open )) && _ezlo_draw
  fi
}

_ezlo_send_env() {
  (( _ezlo_fd >= 0 )) || return
  local us=$'\x1f'
  local msg="ENV${us}${PWD}"
  local k v
  for k in ${(k)parameters[(R)*export*]}; do
    v=${(P)k}
    [[ $k != *$'\n'* && $k != *$us* && $v != *$'\n'* && $v != *$us* ]] && msg+="${us}${k}=${v}"
  done
  _ezlo_send_raw "$msg"
}

_ezlo_send_aliases() {
  (( _ezlo_fd >= 0 )) || return
  local us=$'\x1f'
  local msg="ALIAS"
  local k v
  for k in ${(k)aliases}; do
    v=${aliases[$k]}
    [[ $k != *$'\n'* && $k != *$us* && $v != *$'\n'* && $v != *$us* ]] && msg+="${us}${k}=${v}"
  done
  _ezlo_send_raw "$msg"
}

_ezlo_send_opts() {
  (( _ezlo_fd >= 0 )) || return
  local us=$'\x1f'
  local his=0
  [[ -o hist_ignore_space ]] && his=1
  _ezlo_send_raw "OPTS${us}hist_ignore_space=${his}${us}COLUMNS=${COLUMNS:-80}${us}HISTFILE=${HISTFILE-}"
}

# Sends this shell's daemon-side overrides (`SET` replaces the previous ones); the daemon answers
# with its config values (`S`), which the popup shows as defaults.
_ezlo_send_set() {
  (( _ezlo_fd >= 0 )) || return
  local us=$'\x1f' msg="SET" k
  for k in rows history_rows min_chars signature diagnostics preview; do
    [[ -n ${_ezlo_set[$k]-} ]] && msg+="${us}${k}=${_ezlo_set[$k]}"
  done
  # The daemon lays the rows out narrower when the pane will sit beside them.
  (( _ezlo_pane_force > 0 )) && msg+="${us}pane=on"
  (( _ezlo_pane_force < 0 )) && msg+="${us}pane=off"
  _ezlo_send_raw "$msg"
}

_ezlo_connect() {
  (( _ezlo_fd >= 0 )) && return 0
  if zsocket "$_ezlo_sock" 2>/dev/null; then
    _ezlo_fd=$REPLY
    _ezlo_spawned_daemon=0
    zle -F -w $_ezlo_fd _ezlo_fd_handler
    local us=$'\x1f'
    _ezlo_send_raw "HELLO${us}1${us}zsh${us}$$${us}${COLUMNS:-80}"
    _ezlo_send_env
    _ezlo_send_aliases
    _ezlo_send_opts
    _ezlo_send_set
    return 0
  fi
  if (( ! _ezlo_spawned_daemon )); then
    _ezlo_spawned_daemon=1
    command "$_ezlo_bin" daemon --detach >/dev/null 2>&1 &!
  fi
  return 1
}

_ezlo_fd_handler() {
  local fd=${1:-$_ezlo_fd}
  local -i rc
  _ezlo_getline 0
  rc=$?
  # Readable but no full line yet: wait for the rest (the fd fires again).
  (( rc == 4 )) && return
  if (( rc )); then
    _ezlo_disconnect
    zle && zle -R
    return
  fi
  if _ezlo_consume $fd "$REPLY"; then
    # An empty answer with nothing of ezlo's on screen must not wipe another widget's POSTDISPLAY
    # (e.g. zsh-autosuggestions), as on keystrokes (see _ezlo_on_redraw).
    (( _ezlo_menu_visible || ${#_ezlo_ghost} || _ezlo_drawn || ${#_ezlo_diag_span} || ${#_ezlo_prev_text} || ${#_ezlo_sig} || ${#_ezlo_pane_lines} || ${#_ezlo_explain_lines} || ${#_ezlo_tr_state} || _ezlo_popup_open )) || return
    _ezlo_render
    zle && zle -R
  fi
}

# Parses the responses available on fd $1, starting with line $2.
# Returns 0 if the response to the latest request was applied.
_ezlo_consume() {
  local fd=$1 line=$2
  local us=$'\x1f'
  local -a fields wfields dfields disp hl ins suf kind desc pavail detail dspan dsev dmsg lfields elines estyles espans pfields ptext pkind
  local rid rstart rend rghost nrows rsel rmax ndiags nprev rsig rsig_active rpane_col rrow i wline dline lline pline kv tstate
  local -i trmatch
  local matched=0

  while true; do
    fields=( "${(@ps:$us:)line}" )
    if [[ ${fields[1]-} == "R" ]]; then
      rid=${fields[2]:-0}
      rstart=${fields[3]:-0}
      rend=${fields[4]:-0}
      rghost=${fields[5]-}
      nrows=${fields[6]:-0}
      rsel=${fields[7]:-1}
      rmax=${fields[8]:-0}
      ndiags=${fields[9]:-0}
      nprev=${fields[10]:-0}
      rsig=${fields[11]-}
      rsig_active=""
      [[ -n ${fields[12]-} && -n ${fields[13]-} ]] && rsig_active="${fields[12]} ${fields[13]}"
      rpane_col=${fields[14]:-0}

      # A stale answer (the buffer moved on, as while typing) is skipped unparsed; the empty rows
      # below then fail the same freshness check and nothing is applied.
      if ! { (( rid && rid == _ezlo_tr_id )) && [[ $_ezlo_tr_state == busy && $BUFFER == "$_ezlo_tr_buf" ]]; } &&
         ! { (( _ezlo_enabled && rid == _ezlo_req_id )) && [[ $BUFFER == "$_ezlo_req_buf" ]]; }; then
        for (( i = nrows + ndiags + nprev; i > 0; i-- )); do
          _ezlo_getline || { _ezlo_disconnect; return 1; }
        done
        nrows=0 ndiags=0 nprev=0
      fi

      disp=()
      hl=()
      ins=()
      suf=()
      kind=()
      desc=()
      pavail=()
      detail=()
      for (( i = 1; i <= nrows; i++ )); do
        _ezlo_getline || { _ezlo_disconnect; return 1; }
        wline=$REPLY
        wfields=( "${(@ps:$us:)wline}" )
        [[ ${wfields[1]-} == "W" ]] || continue
        disp+=( "${wfields[2]-}" )
        hl+=( "${wfields[3]-}" )
        ins+=( "${wfields[4]-}" )
        suf+=( "${wfields[5]-}" )
        kind+=( "${wfields[6]-}" )
        desc+=( "${wfields[7]-}" )
        pavail+=( "${wfields[8]-}" )
        detail+=( "${wfields[9]-}" )
      done

      dspan=()
      dsev=()
      dmsg=()
      for (( i = 1; i <= ndiags; i++ )); do
        _ezlo_getline || { _ezlo_disconnect; return 1; }
        dline=$REPLY
        dfields=( "${(@ps:$us:)dline}" )
        [[ ${dfields[1]-} == "D" ]] || continue
        dspan+=( "${dfields[2]:-0} ${dfields[3]:-0}" )
        dsev+=( "${dfields[4]-error}" )
        dmsg+=( "${dfields[5]-}" )
      done

      ptext=()
      pkind=()
      for (( i = 1; i <= nprev; i++ )); do
        _ezlo_getline || { _ezlo_disconnect; return 1; }
        pline=$REPLY
        pfields=( "${(@ps:$us:)pline}" )
        [[ ${pfields[1]-} == "P" ]] || continue
        pkind+=( "${pfields[2]-effect}" )
        ptext+=( "${pfields[3]-}" )
      done

      # The rows of a finished translation (its `T ok` follows) count even with suggestions off.
      trmatch=0
      (( rid && rid == _ezlo_tr_id )) && [[ $_ezlo_tr_state == busy && $BUFFER == "$_ezlo_tr_buf" ]] && trmatch=1
      if (( trmatch )) || { (( _ezlo_enabled && rid == _ezlo_req_id )) && [[ $BUFFER == "$_ezlo_req_buf" ]]; }; then
        _ezlo_resp_id=$rid
        _ezlo_start=$rstart
        _ezlo_end=$rend
        _ezlo_ghost=$rghost
        _ezlo_disp=( "${disp[@]}" )
        _ezlo_hl=( "${hl[@]}" )
        _ezlo_ins=( "${ins[@]}" )
        _ezlo_suf=( "${suf[@]}" )
        _ezlo_kind=( "${kind[@]}" )
        _ezlo_desc=( "${desc[@]}" )
        _ezlo_pane_avail=( "${pavail[@]}" )
        _ezlo_detail=( "${detail[@]}" )
        _ezlo_diag_span=( "${dspan[@]}" )
        _ezlo_diag_sev=( "${dsev[@]}" )
        _ezlo_diag_msg=( "${dmsg[@]}" )
        _ezlo_prev_text=( "${ptext[@]}" )
        _ezlo_prev_kind=( "${pkind[@]}" )
        _ezlo_sig=$rsig
        _ezlo_sig_active=$rsig_active
        # Nothing preselected (the daemon's `rsel` is ignored): Enter keeps running the typed line
        # until Up/Down picks a row. Translation suggestions are the exception: the first one is
        # preselected, since the comment line itself isn't worth running.
        _ezlo_selected=$(( trmatch && ${#disp} ? 1 : 0 ))
        _ezlo_server_rows=$rmax
        _ezlo_pane_col=$rpane_col
        _ezlo_menu_visible=$(( ${#_ezlo_disp} > 0 ? 1 : 0 ))
        _ezlo_pending=0
        matched=1
      fi
    elif [[ ${fields[1]-} == "E" ]]; then
      rid=${fields[2]:-0}
      nrows=${fields[3]:-0}
      elines=()
      estyles=()
      for (( i = 1; i <= nrows; i++ )); do
        _ezlo_getline || { _ezlo_disconnect; return 1; }
        lline=$REPLY
        lfields=( "${(@ps:$us:)lline}" )
        [[ ${lfields[1]-} == "L" ]] || continue
        estyles+=( "${lfields[2]-}" )
        elines+=( "${lfields[3]-}" )
      done
      if (( rid == _ezlo_req_id )) && [[ $BUFFER == "$_ezlo_explain_buf" ]]; then
        _ezlo_explain_id=$rid
        _ezlo_explain_lines=( "${elines[@]}" )
        _ezlo_explain_styles=( "${estyles[@]}" )
        _ezlo_menu_visible=0
        matched=1
      fi
    elif [[ ${fields[1]-} == "V" ]]; then
      rid=${fields[2]:-0}
      rrow=${fields[3]:-0}
      nrows=${fields[4]:-0}
      elines=()
      estyles=()
      espans=()
      for (( i = 1; i <= nrows; i++ )); do
        _ezlo_getline || { _ezlo_disconnect; return 1; }
        lline=$REPLY
        lfields=( "${(@ps:$us:)lline}" )
        [[ ${lfields[1]-} == "L" ]] || continue
        estyles+=( "${lfields[2]-}" )
        elines+=( "${lfields[3]-}" )
        espans+=( "${lfields[4]-}" )
      done
      # Only the reply for the row still selected in the response still shown counts.
      if [[ "$rid:$rrow" == "$_ezlo_pane_req" && "$rid:$rrow" == "${_ezlo_resp_id}:${_ezlo_selected}" ]]; then
        _ezlo_pane_lines=( "${elines[@]}" )
        _ezlo_pane_styles=( "${estyles[@]}" )
        _ezlo_pane_spans=( "${espans[@]}" )
        (( ${fields[5]:-0} > 0 )) && _ezlo_pane_height=${fields[5]}
        [[ $_ezlo_pane_key == "$_ezlo_pane_req" ]] || _ezlo_pane_off=0
        _ezlo_pane_key=$_ezlo_pane_req
        matched=1
      fi
    elif [[ ${fields[1]-} == "T" ]]; then
      rid=${fields[2]:-0}
      tstate=${fields[3]-}
      nrows=${fields[4]:-0}
      elines=()
      for (( i = 1; i <= nrows; i++ )); do
        _ezlo_getline || { _ezlo_disconnect; return 1; }
        lline=$REPLY
        lfields=( "${(@ps:$us:)lline}" )
        [[ ${lfields[1]-} == "L" ]] || continue
        elines+=( "${lfields[3]-}" )
      done
      # Only the running translation of the line still being edited counts.
      if (( rid && rid == _ezlo_tr_id )) && [[ $_ezlo_tr_state == busy && $BUFFER == "$_ezlo_tr_buf" ]]; then
        case $tstate in
          ok)
            # The rows came in the `R` just before; none means a daemon predating suggestion rows.
            if [[ ${_ezlo_kind[1]-} == translated ]]; then
              _ezlo_translate_done
            else
              _ezlo_tr_state=err
              _ezlo_tr_msg="no suggestions received (outdated daemon? run: ezlo daemon --stop)"
            fi
            ;;
          *) _ezlo_tr_state=err; _ezlo_tr_msg=${elines[1]:-translation failed} ;;
        esac
        matched=1
      fi
    elif [[ ${fields[1]-} == "S" ]]; then
      _ezlo_cfg=()
      for kv in "${(@)fields[2,-1]}"; do
        [[ $kv == *=* ]] && _ezlo_cfg[${kv%%=*}]=${kv#*=}
      done
      (( _ezlo_popup_open )) && matched=1
    fi

    _ezlo_getline 0 || break
    line=$REPLY
  done

  (( matched ))
}

# Brings the state up to date with the current buffer, briefly blocking for the daemon's answer, so
# menu actions never act on rows (or replacement ranges) computed for an older buffer. Needed when
# keys arrive faster than the daemon answers, or before zle got to redraw (typeahead).
_ezlo_sync() {
  _ezlo_on_redraw
  local line
  local -i applied=0
  while (( _ezlo_pending && _ezlo_fd >= 0 )); do
    if ! _ezlo_getline 0.5; then
      _ezlo_pending=0
      break
    fi
    _ezlo_consume $_ezlo_fd "$REPLY" && applied=1
  done
  (( applied )) && _ezlo_render
  return 0
}

_ezlo_render() {
  _ezlo_pane_update
  _ezlo_draw
  _ezlo_drawn=$(( ${#POSTDISPLAY} > 0 || ${#_ezlo_diag_span} > 0 || ${#_ezlo_prev_text} > 0 || ${#_ezlo_sig} > 0 || ${#_ezlo_pane_lines} > 0 ))
}

# Succeeds when the selected row's preview pane should be on screen: rows the daemon marks `auto`
# unless the pane was turned off, and any previewable row once it was turned on (Alt-E menu).
_ezlo_pane_wanted() {
  (( _ezlo_menu_visible && ${#_ezlo_disp} && _ezlo_selected > 0 && ! ${#_ezlo_explain_lines} && ! _ezlo_popup_open )) || return 1
  local avail=${_ezlo_pane_avail[$_ezlo_selected]-}
  [[ -n $avail ]] || return 1
  (( _ezlo_pane_force > 0 )) && return 0
  (( _ezlo_pane_force == 0 )) && [[ $avail == auto ]]
}

# Asks the daemon (`PANE`, answered asynchronously with `V`) for the selected row's preview once per
# response/row; drops the pane when it isn't wanted. The previous lines stay until the reply lands,
# so moving between rows doesn't make the dropdown jump.
_ezlo_pane_update() {
  if ! _ezlo_pane_wanted; then
    _ezlo_pane_lines=()
    _ezlo_pane_styles=()
    _ezlo_pane_spans=()
    _ezlo_pane_key=""
    _ezlo_pane_req=""
    _ezlo_pane_off=0
    return 0
  fi
  local key="${_ezlo_resp_id}:${_ezlo_selected}"
  [[ $key == "$_ezlo_pane_req" ]] && return 0
  _ezlo_pane_req=$key
  local us=$'\x1f'
  _ezlo_send_raw "PANE${us}${_ezlo_resp_id}${us}${_ezlo_selected}${us}${_ezlo_ins[$_ezlo_selected]-}"
}

# Draws ezlo's overlay. The parts build the plain locals _ezlo_pd / _ezlo_rh, assigned to the zle
# specials once at the end: every `+=` or `${#…}` on POSTDISPLAY / region_highlight converts the
# whole value, which made a frame quadratic in the dropdown size.
_ezlo_draw() {
  local _ezlo_pd=""
  local -a _ezlo_rh=()
  _ezlo_draw_parts
  POSTDISPLAY=$_ezlo_pd
  region_highlight=( ${region_highlight:#*memo=ezlo} "${_ezlo_rh[@]}" )
}

_ezlo_draw_parts() {
  local buf_len=${#BUFFER}
  if (( _ezlo_popup_open )); then
    _ezlo_draw_popup
    return
  fi
  if (( ${#_ezlo_explain_lines} )); then
    _ezlo_draw_explain
    return
  fi
  if [[ -n $_ezlo_tr_state ]]; then
    _ezlo_draw_translate
    return
  fi
  # Ghost text: the popup's override, else $EZLO_GHOST (auto: hidden when zsh-autosuggestions runs).
  local ghost_mode=${_ezlo_set[ghost]:-${EZLO_GHOST:-auto}}
  if [[ -n $_ezlo_ghost && $CURSOR == $buf_len && $ghost_mode != "off" ]]; then
    if [[ $ghost_mode == "on" ]] || (( ! $+_zsh_autosuggest_strategy )); then
      _ezlo_pd=$_ezlo_ghost
      _ezlo_rh+=( "$buf_len $(( buf_len + ${#_ezlo_ghost} )) fg=8 memo=ezlo" )
    fi
  fi

  local -i di
  local dstyle
  for (( di = 1; di <= ${#_ezlo_diag_span}; di++ )); do
    if [[ ${_ezlo_diag_sev[$di]-} == "warning" ]]; then
      dstyle="fg=yellow,underline"
    else
      dstyle="fg=red,underline"
    fi
    _ezlo_rh+=( "${_ezlo_diag_span[$di]} ${dstyle} memo=ezlo" )
  done

  # Without a dropdown: the effect lines (in full: they compete with no suggestion), the alert line,
  # then the status line, in the same order as with one.
  if (( ! _ezlo_menu_visible || ${#_ezlo_disp} == 0 )); then
    _ezlo_draw_preview
    _ezlo_draw_alert 0
    _ezlo_draw_status "" ""
    return
  fi

  local total=${#_ezlo_disp}
  # Window height: the popup's override > $EZLO_MAX_ROWS (read live) > config `[complete.dropdown]
  # rows` (sent by the daemon) > 6, capped so the prompt line, the rows and the status line fit.
  local -i win=${_ezlo_set[rows]:-${EZLO_MAX_ROWS:-0}}
  (( win > 0 )) || win=$_ezlo_server_rows
  (( win > 0 )) || win=6
  # The configured height is also the preview pane's ceiling, whatever rows are shown.
  local -i win_max=$win
  # Below the rows: one status line at most (see _ezlo_draw_status); above them the effect lines
  # and the alert line. The preview pane gives up lines first: at least 3 dropdown rows stay when
  # the terminal is short. Beside the rows (_ezlo_pane_col) it takes no lines of its own, below
  # them (pane turned on) it does.
  local -i reserved=2
  # Effect lines (blast radius, values, dry runs) stay visible above the dropdown too, up to
  # _ezlo_prev_max of them; warnings go to the alert line instead.
  local -i prev_n=0
  local pk
  for pk in "${_ezlo_prev_kind[@]}"; do [[ $pk != warning ]] && (( prev_n++ )); done
  (( prev_n > _ezlo_prev_max )) && prev_n=$_ezlo_prev_max
  (( reserved += prev_n ))
  _ezlo_has_alert 1 && (( reserved++ ))
  local -i pane_total=${#_ezlo_pane_lines} pane_right=0 pane_n=0 pane_head=0
  (( pane_total && _ezlo_pane_col > 0 && _ezlo_pane_col < ${COLUMNS:-80} - 4 )) && pane_right=1
  # A truncated selected row shows in full on top of a pane beside the rows, else in the status line.
  local detail=${_ezlo_detail[$_ezlo_selected]-}
  (( pane_right )) && [[ -n $detail ]] && pane_head=1
  # Pane height: _ezlo_pane_height lines, beside the rows at least as tall as the dropdown; never
  # taller than its content, and a short terminal keeps room for the rows.
  local -i pane_max=$(( LINES > 0 ? LINES - reserved - 4 : 999 ))
  (( pane_max > win_max )) && pane_max=$win_max
  if (( pane_total && ! pane_right )); then
    pane_n=$(( pane_total < _ezlo_pane_height ? pane_total : _ezlo_pane_height ))
    (( pane_n > pane_max )) && pane_n=$pane_max
    (( pane_n < 0 )) && pane_n=0
    (( reserved += pane_n ))
  fi
  (( LINES > reserved + 1 && win > LINES - reserved )) && win=$(( LINES - reserved ))
  local first=1
  if (( _ezlo_selected > win )); then
    first=$(( _ezlo_selected - win + 1 ))
  fi
  local last=$(( first + win - 1 ))
  (( last > total )) && last=$total
  if (( pane_right )); then
    pane_n=$(( last - first + 1 > _ezlo_pane_height ? last - first + 1 : _ezlo_pane_height ))
    (( pane_n > pane_total + pane_head )) && pane_n=$(( pane_total + pane_head ))
    (( pane_n > pane_max )) && pane_n=$pane_max
    (( pane_n < 0 )) && pane_n=0
  fi

  # The pane's lines: the pinned detail line, then the scrolled window of the preview, with a
  # thumb on the gutter (pane lines pane_thumb_from..pane_thumb_to, 0-based) when it doesn't fit.
  local -a pane_text=() pane_sty=() pane_spn=() reply=()
  local -i pane_view=$(( pane_n > pane_head ? pane_n - pane_head : 0 )) pane_thumb_from=-1 pane_thumb_to=-1
  (( _ezlo_pane_off > pane_total - pane_view )) && _ezlo_pane_off=$(( pane_total - pane_view ))
  (( _ezlo_pane_off < 0 )) && _ezlo_pane_off=0
  _ezlo_pane_view=$pane_view
  if (( pane_head )); then
    pane_text=( "$detail" )
    pane_sty=( bold )
    pane_spn=( "" )
    detail=""
  fi
  if (( pane_view )); then
    pane_text+=( "${(@)_ezlo_pane_lines[_ezlo_pane_off+1,_ezlo_pane_off+pane_view]}" )
    pane_sty+=( "${(@)_ezlo_pane_styles[_ezlo_pane_off+1,_ezlo_pane_off+pane_view]}" )
    pane_spn+=( "${(@)_ezlo_pane_spans[_ezlo_pane_off+1,_ezlo_pane_off+pane_view]}" )
    _ezlo_scroll_thumb $pane_total $pane_view $_ezlo_pane_off
    if (( reply[1] >= 0 )); then
      pane_thumb_from=$(( reply[1] + pane_head ))
      pane_thumb_to=$(( reply[2] + pane_head ))
    fi
  fi

  # Scrollbar thumb in the rows' reserved right-edge gutter cell when not every row fits: rows
  # thumb_from..thumb_to (window-relative, 0-based) get a `┃`.
  _ezlo_scroll_thumb $total $win $(( first - 1 ))
  local -i thumb_from=${reply[1]} thumb_to=${reply[2]}
  local thumb_style
  zstyle -s ':ezlo:style' scrollbar thumb_style || thumb_style='fg=8'

  local sel_style
  zstyle -s ':ezlo:style' selected sel_style || sel_style='bg=236'

  # Right under the prompt, at a spot that doesn't depend on how many rows match: the effect
  # lines, then the alert line (diagnostic / effect warning).
  _ezlo_draw_preview 1 $prev_n
  _ezlo_draw_alert 1

  local i row row_start spec range style rfrom rto
  for (( i = first; i <= last; i++ )); do
    row=${_ezlo_disp[$i]}
    (( i == _ezlo_selected )) && row="›${row[2,-1]}"
    local -i thumb=$(( i - first >= thumb_from && i - first <= thumb_to ))
    (( thumb )) && row="${row[1,-2]}┃"
    _ezlo_pd+=$'\n'
    row_start=$(( buf_len + ${#_ezlo_pd} ))
    _ezlo_pd+=$row

    # zsh doesn't merge overlapping region_highlight entries (a later fg-only entry resets bg),
    # so the selected row's style is folded into every segment.
    local extra=""
    if (( i == _ezlo_selected )); then
      extra=",${sel_style},bold"
      _ezlo_rh+=( "$row_start $(( row_start + ${#row} )) ${sel_style} memo=ezlo" )
      _ezlo_rh+=( "$row_start $(( row_start + 1 )) fg=cyan${extra} memo=ezlo" )
    fi
    for spec in ${=_ezlo_hl[$i]}; do
      range=${spec%%:*}
      style=${spec#*:}
      rfrom=${range%%-*}
      rto=${range#*-}
      _ezlo_rh+=( "$(( row_start + rfrom )) $(( row_start + rto )) ${style}${extra} memo=ezlo" )
    done
    (( thumb )) && _ezlo_rh+=( "$(( row_start + ${#row} - 1 )) $(( row_start + ${#row} )) ${thumb_style}${extra} memo=ezlo" )
    (( pane_right )) && _ezlo_draw_pane_cell $(( i - first + 1 )) ${(m)#row}
  done
  # A pane taller than the rows continues below them, its left side blank.
  if (( pane_right )); then
    for (( i = last - first + 2; i <= pane_n; i++ )); do
      _ezlo_pd+=$'\n'
      _ezlo_draw_pane_cell $i 0
    done
  fi

  local count=""
  (( total > win )) && count="${${_ezlo_selected:#0}:--}/${total}"
  # A pane longer than its window says where it is and how to scroll it.
  if (( pane_view && pane_total > pane_view )); then
    local pane_pos="pane $(( _ezlo_pane_off + 1 ))-$(( _ezlo_pane_off + pane_view ))/${pane_total} ⇅PgUp/PgDn"
    count=${count:+$count · }$pane_pos
  fi
  _ezlo_draw_status "$detail" "$count"
  (( pane_right )) || _ezlo_draw_pane $pane_n
}

# Succeeds when there is an alert line to draw (see _ezlo_draw_alert); $1 = 1: dropdown open.
_ezlo_has_alert() {
  [[ -n ${_ezlo_diag_msg[1]-} ]] || (( $1 && ${_ezlo_prev_kind[(I)warning]} ))
}

# The alert line, above the dropdown right after the effect lines: the first diagnostic, else
# (dropdown open, $1 = 1) the last effect warning (the effect lines skip warnings then). Nothing to
# say: no line.
_ezlo_draw_alert() {
  local -i menu=$1 w=0
  local text style
  if [[ -n ${_ezlo_diag_msg[1]-} ]]; then
    text="⚠ ${_ezlo_diag_msg[1]}" style='fg=red'
    [[ ${_ezlo_diag_sev[1]-} == warning ]] && style='fg=yellow'
  else
    (( menu )) && w=${_ezlo_prev_kind[(I)warning]}
    (( w )) || return 0
    text="⚠ ${_ezlo_prev_text[$w]}" style='fg=yellow'
  fi
  local -i maxw=$(( ${COLUMNS:-80} - 1 ))
  local line="  ${text}"
  (( ${#line} > maxw )) && line="${line[1,$(( maxw - 1 ))]}…"
  _ezlo_pd+=$'\n'
  local -i row_start=$(( ${#BUFFER} + ${#_ezlo_pd} ))
  _ezlo_pd+=$line
  _ezlo_rh+=( "$(( row_start + 2 )) $(( row_start + ${#line} )) ${style} memo=ezlo" )
}

# The one metadata line below the dropdown (or the effect / alert lines without one): the
# selected row's full text ($1, when truncated), else the positional signature, with the scroll
# position ($2) right-aligned. Nothing to say: no line. Styles: `zstyle ':ezlo:style' signature`
# (default fg=8), `signature-active` (default fg=cyan,bold,underline), `detail` (default fg=cyan).
_ezlo_draw_status() {
  local detail=$1 count=$2
  local text="" style="" kind=""
  if [[ -n $detail ]]; then
    text=$detail kind=detail
    zstyle -s ':ezlo:style' detail style || style='fg=cyan'
  elif [[ -n $_ezlo_sig ]]; then
    text=$_ezlo_sig kind=sig
    zstyle -s ':ezlo:style' signature style || style='fg=8'
  fi
  [[ -n $text || -n $count ]] || return 0

  local buf_len=${#BUFFER}
  local -i maxw=$(( ${COLUMNS:-80} - 1 ))
  local -i room=$(( maxw - (${#count} ? ${#count} + 1 : 0) ))
  local line="  ${text}"
  (( ${#line} > room )) && line="${line[1,$(( room - 1 ))]}…"
  _ezlo_pd+=$'\n'
  local -i row_start=$(( buf_len + ${#_ezlo_pd} ))
  _ezlo_pd+=$line
  [[ -n $text ]] && _ezlo_rh+=( "$(( row_start + 2 )) $(( row_start + ${#line} )) ${style} memo=ezlo" )
  if [[ $kind == sig && -n $_ezlo_sig_active ]]; then
    local act_style
    zstyle -s ':ezlo:style' signature-active act_style || act_style='fg=cyan,bold,underline'
    local -i afrom=$(( ${_ezlo_sig_active%% *} + 2 )) ato=$(( ${_ezlo_sig_active##* } + 2 ))
    (( ato > ${#line} )) && ato=${#line}
    (( afrom < ato )) && _ezlo_rh+=( "$(( row_start + afrom )) $(( row_start + ato )) ${act_style} memo=ezlo" )
  fi
  if [[ -n $count ]]; then
    local pad
    printf -v pad '%*s' $(( maxw - ${#line} - ${#count} > 1 ? maxw - ${#line} - ${#count} : 1 )) ''
    _ezlo_pd+="${pad}${count}"
    _ezlo_rh+=( "$(( buf_len + ${#_ezlo_pd} - ${#count} )) $(( buf_len + ${#_ezlo_pd} )) fg=8 memo=ezlo" )
  fi
}

# Thumb of a 1-cell scrollbar for a $2-line window at 0-based offset $3 into $1 lines (mirrors
# `layout::scroll_thumb`): reply=( first last ), window-relative and 0-based; ( -1 -1 ) when all fit.
_ezlo_scroll_thumb() {
  local -i total=$1 win=$2 off=$3
  reply=( -1 -1 )
  (( win > 0 && total > win )) || return 0
  local -i len=$(( (win * win + total / 2) / total )) span=$(( total - win ))
  (( len < 1 )) && len=1
  (( len > win )) && len=$win
  (( off > span )) && off=$span
  local -i from=$(( (off * (win - len) + span / 2) / span ))
  reply=( $from $(( from + len - 1 )) )
}

# Gutter glyph of pane line $1 (1-based): `┃` on the caller's pane_thumb_from..pane_thumb_to, blank
# elsewhere (no separator bar; the cell only carries the scroll thumb).
_ezlo_pane_gutter() {
  (( $1 - 1 >= pane_thumb_from && $1 - 1 <= pane_thumb_to )) && REPLY='┃' || REPLY=' '
}

# One line of the pane beside the dropdown: pads the current _ezlo_pd line ($2 cells wide so
# far) to _ezlo_pane_col, then draws the gutter and pane line $1 of the caller's `pane_text` /
# `pane_sty` / `pane_spn` (empty past the pane's end), truncated to the terminal width.
_ezlo_draw_pane_cell() {
  local -i n=$1 used=$2
  local -i room=$(( ${COLUMNS:-80} - 1 - _ezlo_pane_col - 2 ))
  (( room > 0 )) || return 0
  local text=${pane_text[$n]-} pad gutter_style style REPLY
  (( ${#text} > room )) && text="${text[1,$(( room - 1 ))]}…"
  zstyle -s ':ezlo:style' pane gutter_style || gutter_style='fg=8'
  printf -v pad '%*s' $(( _ezlo_pane_col > used ? _ezlo_pane_col - used : 1 )) ''
  _ezlo_pd+=$pad
  local -i at=$(( ${#BUFFER} + ${#_ezlo_pd} ))
  _ezlo_pane_gutter $n
  _ezlo_pd+="$REPLY ${text}"
  _ezlo_rh+=( "$at $(( at + 1 )) ${gutter_style} memo=ezlo" )
  style=${pane_sty[$n]-}
  [[ -n $style && -n $text ]] && _ezlo_rh+=( "$(( at + 2 )) $(( at + 2 + ${#text} )) ${style} memo=ezlo" )
  _ezlo_pane_spans_rh $(( at + 2 )) ${#text} "${pane_spn[$n]-}"
}

# region_highlight entries of a pane line's colored runs ($3, `from-to:style …`), for the line's
# text starting at buffer offset $1 and cut to $2 chars.
_ezlo_pane_spans_rh() {
  local -i base=$1 len=$2 from to
  local spec range
  for spec in ${=3}; do
    range=${spec%%:*}
    from=${range%%-*} to=${range#*-}
    (( to > len )) && to=$len
    (( from < to )) && _ezlo_rh+=( "$(( base + from )) $(( base + to )) ${spec#*:} memo=ezlo" )
  done
}

# Candidate preview pane below the footer (narrow terminals, `position = "bottom"`): the first $1
# lines of the caller's `pane_text` / `pane_sty` (the scrolled window of the selected row's
# preview: type `preview` command, file head, directory listing), behind a gutter. Styles:
# `zstyle ':ezlo:style' pane` (gutter + text default, default fg=8 for the gutter); per-line styles
# come from the daemon.
_ezlo_draw_pane() {
  local -i n=${1:-${#pane_text}}
  (( n > 0 )) || return 0
  local buf_len=${#BUFFER}
  local maxw=$(( ${COLUMNS:-80} - 1 ))
  local gutter_style REPLY
  zstyle -s ':ezlo:style' pane gutter_style || gutter_style='fg=8'
  local -i i row_start
  local text style
  for (( i = 1; i <= n; i++ )); do
    _ezlo_pane_gutter $i
    text="  $REPLY ${pane_text[$i]-}"
    (( ${#text} > maxw )) && text="${text[1,$(( maxw - 1 ))]}…"
    _ezlo_pd+=$'\n'
    row_start=$(( buf_len + ${#_ezlo_pd} ))
    _ezlo_pd+=$text
    _ezlo_rh+=( "$(( row_start + 2 )) $(( row_start + 3 )) ${gutter_style} memo=ezlo" )
    style=${pane_sty[$i]-}
    [[ -n $style ]] && _ezlo_rh+=( "$(( row_start + 4 )) $(( row_start + ${#text} )) ${style} memo=ezlo" )
    _ezlo_pane_spans_rh $(( row_start + 4 )) $(( ${#text} - 4 )) "${pane_spn[$i]-}"
  done
}

# Effect preview: one line per blast-radius summary / value / dry-run output line, first below the
# prompt. With $1 = 1 (dropdown open) warnings are skipped (the alert line shows them)
# and at most $2 lines are drawn. Styles: `zstyle ':ezlo:style' preview` (blast radius, default
# fg=magenta), `value` (what a read-only command would print, default fg=green), `info` (low-key
# summaries like `cd`/`ls` counts, default fg=8), `dryrun` (spec `preview $ ...` output, default
# fg=8); unmatched globs are always yellow.
_ezlo_draw_preview() {
  (( ${#_ezlo_prev_text} )) || return 0
  local -i menu=${1:-0} max=${2:-999}
  local buf_len=${#BUFFER}
  local maxw=$(( ${COLUMNS:-80} - 1 ))
  local effect_style dryrun_style value_style info_style style text
  zstyle -s ':ezlo:style' preview effect_style || effect_style='fg=magenta'
  zstyle -s ':ezlo:style' dryrun dryrun_style || dryrun_style='fg=8'
  zstyle -s ':ezlo:style' value value_style || value_style='fg=green'
  zstyle -s ':ezlo:style' info info_style || info_style='fg=8'
  local -i i row_start drawn=0
  for (( i = 1; i <= ${#_ezlo_prev_text}; i++ )); do
    (( menu )) && [[ ${_ezlo_prev_kind[$i]-} == warning ]] && continue
    (( drawn >= max )) && break
    (( drawn++ ))
    case ${_ezlo_prev_kind[$i]-} in
      warning) style='fg=yellow'; text="  ⚠ ${_ezlo_prev_text[$i]}" ;;
      dryrun) style=$dryrun_style; text="  ⇢ ${_ezlo_prev_text[$i]}" ;;
      value) style=$value_style; text="  = ${_ezlo_prev_text[$i]}" ;;
      info) style=$info_style; text="  · ${_ezlo_prev_text[$i]}" ;;
      *) style=$effect_style; text="  ⇢ ${_ezlo_prev_text[$i]}" ;;
    esac
    (( ${#text} > maxw )) && text="${text[1,$(( maxw - 1 ))]}…"
    _ezlo_pd+=$'\n'
    row_start=$(( buf_len + ${#_ezlo_pd} ))
    _ezlo_pd+=$text
    _ezlo_rh+=( "$(( row_start + 2 )) $(( row_start + ${#text} )) ${style} memo=ezlo" )
  done
}

# Sets REPLY to popup row $1's value when this shell overrides nothing.
_ezlo_popup_default() {
  case $1 in
    enabled) REPLY=on ;;
    rows) REPLY=${EZLO_MAX_ROWS:-${_ezlo_cfg[rows]:-6}} ;;
    history_rows) REPLY=${_ezlo_cfg[history_rows]:-3} ;;
    min_chars) REPLY=${_ezlo_cfg[min_chars]:-3} ;;
    ghost) REPLY=${EZLO_GHOST:-auto} ;;
    signature|diagnostics|preview) REPLY=on; [[ ${_ezlo_cfg[$1]-1} == 0 ]] && REPLY=off ;;
    pane) REPLY=auto ;;
    *) REPLY="" ;;
  esac
}

# Sets REPLY to popup row $1's current value.
_ezlo_popup_value() {
  case $1 in
    enabled) (( _ezlo_enabled )) && REPLY=on || REPLY=off ;;
    pane) case $_ezlo_pane_force in 1) REPLY=on ;; -1) REPLY=off ;; *) REPLY=auto ;; esac ;;
    reset) REPLY="" ;;
    *) if [[ -n ${_ezlo_set[$1]-} ]]; then REPLY=${_ezlo_set[$1]}; else _ezlo_popup_default $1; fi ;;
  esac
}

# Stores override $1=$2, dropping it when it equals the default.
_ezlo_popup_store() {
  local REPLY
  _ezlo_popup_default $1
  if [[ $2 == "$REPLY" ]]; then
    unset "_ezlo_set[$1]"
  else
    _ezlo_set[$1]=$2
  fi
}

# Changes popup row $1 by $2: +1/-1 (Right/Left) steps numbers and cycles choices, 0 (Space/Enter)
# toggles, cycles forward or runs the row's action. Applies right away and re-queries the line.
_ezlo_popup_change() {
  local key=$1 REPLY val
  local -i dir=$2 n lo hi i
  _ezlo_popup_value $key
  val=$REPLY
  case $key in
    reset)
      (( dir )) && return 0
      _ezlo_set=()
      _ezlo_pane_force=0
      _ezlo_enabled=1
      ;;
    enabled)
      if [[ $val == on ]]; then
        _ezlo_enabled=0
        _ezlo_clear_state
      else
        _ezlo_enabled=1
      fi
      ;;
    signature|diagnostics|preview)
      [[ $val == on ]] && _ezlo_popup_store $key off || _ezlo_popup_store $key on
      ;;
    ghost|pane)
      local -a choices=( auto on off )
      i=${choices[(I)$val]}
      (( i )) || i=1
      i=$(( (i - 1 + (dir < 0 ? 2 : 1)) % 3 + 1 ))
      if [[ $key == pane ]]; then
        case ${choices[$i]} in on) _ezlo_pane_force=1 ;; off) _ezlo_pane_force=-1 ;; *) _ezlo_pane_force=0 ;; esac
      else
        _ezlo_popup_store ghost ${choices[$i]}
      fi
      ;;
    rows|history_rows|min_chars)
      case $key in
        rows) lo=1 hi=30 ;;
        history_rows) lo=0 hi=10 ;;
        *) lo=0 hi=9 ;;
      esac
      (( dir )) || dir=1
      n=$(( val + dir ))
      (( n < lo )) && n=$lo
      (( n > hi )) && n=$hi
      _ezlo_popup_store $key $n
      ;;
  esac
  _ezlo_send_set
  # Re-query the line with the new settings on the next redraw.
  _ezlo_last_buf=""
  _ezlo_last_cur=-1
}

# Alt-E settings menu, left-aligned below the prompt like the dropdown: a title, one row per setting
# (values that differ from the default highlighted) and a key-hint footer. Styles: `zstyle
# ':ezlo:style' popup-border` (title suffix and footer, default fg=8), `popup-changed` (default
# fg=yellow,bold), `selected` (as the dropdown).
_ezlo_draw_popup() {
  local buf_len=${#BUFFER}
  local -i lw=17 vw=8
  local hint_style changed_style sel_style
  zstyle -s ':ezlo:style' popup-border hint_style || hint_style='fg=8'
  zstyle -s ':ezlo:style' popup-changed changed_style || changed_style='fg=yellow,bold'
  zstyle -s ':ezlo:style' selected sel_style || sel_style='bg=236'
  local title="  ezlo settings · this shell" foot="  ↑↓ move  ←→ change  esc close"
  local line key label val def shown mark extra REPLY
  local -i i row_start vstart

  _ezlo_pd+=$'\n'
  row_start=$(( buf_len + ${#_ezlo_pd} ))
  _ezlo_pd+=$title
  _ezlo_rh+=( "$(( row_start + 2 )) $(( row_start + 15 )) bold memo=ezlo" )
  _ezlo_rh+=( "$(( row_start + 15 )) $(( row_start + ${#title} )) ${hint_style} memo=ezlo" )

  # Rows mirror the dropdown's: the `›` marker in column 0, the label from column 2.
  for (( i = 1; i <= ${#_ezlo_popup_keys}; i++ )); do
    key=${_ezlo_popup_keys[$i]}
    label=${_ezlo_popup_labels[$key]}
    _ezlo_popup_value $key
    val=$REPLY
    _ezlo_popup_default $key
    def=$REPLY
    shown=$val mark=" " extra=""
    if (( i == _ezlo_popup_sel )); then
      mark="›"
      extra=",${sel_style},bold"
      [[ -n $val ]] && shown="‹ ${val} ›"
    fi
    line="${mark} ${(r:lw:)label} ${(l:vw:)shown} "
    _ezlo_pd+=$'\n'
    row_start=$(( buf_len + ${#_ezlo_pd} ))
    _ezlo_pd+=$line
    if (( i == _ezlo_popup_sel )); then
      _ezlo_rh+=( "$row_start $(( row_start + ${#line} )) ${sel_style},bold memo=ezlo" )
      _ezlo_rh+=( "$row_start $(( row_start + 1 )) fg=cyan${extra} memo=ezlo" )
    fi
    if [[ $key != reset && $val != "$def" ]]; then
      vstart=$(( 3 + lw + vw - ${#shown} ))
      _ezlo_rh+=( "$(( row_start + vstart )) $(( row_start + 3 + lw + vw )) ${changed_style}${extra} memo=ezlo" )
    fi
  done

  _ezlo_pd+=$'\n'
  row_start=$(( buf_len + ${#_ezlo_pd} ))
  _ezlo_pd+=$foot
  _ezlo_rh+=( "$row_start $(( row_start + ${#foot} )) ${hint_style} memo=ezlo" )
}

# Alt-H overlay: one explanation line per row below the prompt, each truncated to the terminal width.
_ezlo_draw_explain() {
  local buf_len=${#BUFFER}
  local maxw=$(( ${COLUMNS:-80} - 1 ))
  local -i i n=${#_ezlo_explain_lines} row_start
  (( LINES > 3 && n > LINES - 2 )) && n=$(( LINES - 2 ))
  local text style
  for (( i = 1; i <= n; i++ )); do
    text=${_ezlo_explain_lines[$i]}
    (( ${#text} > maxw )) && text="${text[1,$(( maxw - 1 ))]}…"
    _ezlo_pd+=$'\n'
    row_start=$(( buf_len + ${#_ezlo_pd} ))
    _ezlo_pd+=$text
    style=${_ezlo_explain_styles[$i]-}
    [[ -n $style ]] && _ezlo_rh+=( "$row_start $(( row_start + ${#text} )) ${style} memo=ezlo" )
  done
}

# Translation status below the prompt while the translator runs, else its error. Static on purpose:
# animating it needs socket traffic, which restarts zle's KEYTIMEOUT and so would swallow a lone Esc.
# Styles: `zstyle ':ezlo:style' translate` (default fg=cyan) and `translate-error` (default fg=red).
_ezlo_draw_translate() {
  local buf_len=${#BUFFER}
  local maxw=$(( ${COLUMNS:-80} - 1 ))
  local text style
  if [[ $_ezlo_tr_state == busy ]]; then
    zstyle -s ':ezlo:style' translate style || style='fg=cyan'
    text="  ⋯ translating… (esc cancels)"
  else
    zstyle -s ':ezlo:style' translate-error style || style='fg=red'
    text="  ✗ translate: ${_ezlo_tr_msg}"
  fi
  (( ${#text} > maxw )) && text="${text[1,$(( maxw - 1 ))]}…"
  _ezlo_pd+=$'\n'
  local -i row_start=$(( buf_len + ${#_ezlo_pd} ))
  _ezlo_pd+=$text
  _ezlo_rh+=( "$(( row_start + 2 )) $(( row_start + ${#text} )) ${style} memo=ezlo" )
}

# Forgets the translation state, telling the daemon to kill a running translator.
_ezlo_translate_cancel() {
  local -i id=$_ezlo_tr_id
  local st=$_ezlo_tr_state
  _ezlo_tr_id=0
  _ezlo_tr_state=""
  _ezlo_tr_msg=""
  _ezlo_tr_buf=""
  [[ $st == busy ]] && (( id )) && _ezlo_send_raw "TRANSLATE"$'\x1f'"${id}"$'\x1f'
  return 0
}

# Drops the translation (status or error) once its line was edited.
_ezlo_translate_check() {
  [[ -n $_ezlo_tr_state && $BUFFER != "$_ezlo_tr_buf" ]] || return 0
  _ezlo_translate_cancel
  if (( ! _ezlo_enabled )); then
    POSTDISPLAY=""
    region_highlight=( ${region_highlight:#*memo=ezlo} )
    _ezlo_drawn=0
  fi
}

# Ends a successful translation: its suggestions already arrived as dropdown rows (`R`), so only
# the status line goes away; the line itself stays until a row is accepted.
_ezlo_translate_done() {
  _ezlo_tr_id=0
  _ezlo_tr_state=""
  _ezlo_tr_msg=""
  _ezlo_tr_buf=""
}

# Drops the explanation once the line it describes was edited (or the cursor moved).
_ezlo_explain_check() {
  (( ${#_ezlo_explain_lines} )) || return 0
  [[ $BUFFER == "$_ezlo_explain_buf" && $CURSOR == $_ezlo_explain_cur ]] && return 0
  _ezlo_explain_lines=()
  _ezlo_explain_styles=()
  if (( ! _ezlo_enabled )); then
    POSTDISPLAY=""
    region_highlight=( ${region_highlight:#*memo=ezlo} )
    _ezlo_drawn=0
  fi
}

_ezlo_on_redraw() {
  _ezlo_explain_check
  _ezlo_translate_check
  (( _ezlo_enabled )) || return 0
  if [[ $BUFFER == "$_ezlo_last_buf" && $CURSOR == $_ezlo_last_cur ]]; then
    # Highlighters (e.g. fast-syntax-highlighting) may rebuild region_highlight; re-assert ours.
    if (( (_ezlo_menu_visible || ${#_ezlo_ghost} || ${#_ezlo_diag_span} || ${#_ezlo_prev_text} || ${#_ezlo_sig} || ${#_ezlo_pane_lines} || ${#_ezlo_explain_lines} || ${#_ezlo_tr_state} || _ezlo_popup_open) && ! ${region_highlight[(I)*memo=ezlo]} )); then
      _ezlo_render
    fi
    return
  fi
  local old_buf=$_ezlo_last_buf
  local -i old_cur=$_ezlo_last_cur
  _ezlo_last_buf=$BUFFER
  _ezlo_last_cur=$CURSOR
  # Editing drops the selection: the rows still shown belong to the old line, and Enter must not
  # accept one of them.
  _ezlo_selected=0
  # A Tab list belongs to the line it completed: editing closes it (suggestions come back).
  (( _ezlo_native )) && _ezlo_clear_state

  if (( _ezlo_hist_nav )); then
    if [[ $BUFFER == "$_ezlo_hist_buf" ]]; then
      _ezlo_clear_state
      return
    fi
    _ezlo_hist_nav=0
  fi

  if [[ -z $BUFFER || $BUFFER == *$'\n'* || $BUFFER == *$'\x1f'* ]]; then
    _ezlo_clear_state
    return
  fi

  # Re-render right away instead of waiting for the daemon: POSTDISPLAY and region_highlight are
  # anchored to the buffer length, so leaving them stale shows a mis-highlighted frame per keystroke.
  # The previous rows stay until the new response lands, keeping the menu height stable.
  # Skipped when ezlo has nothing on screen, so another widget's POSTDISPLAY isn't wiped.
  _ezlo_predict_ghost "$old_buf" $old_cur
  (( _ezlo_menu_visible || ${#_ezlo_ghost} || _ezlo_drawn )) && _ezlo_render

  _ezlo_connect || return
  (( _ezlo_req_id++ ))
  _ezlo_req_buf=$BUFFER
  _ezlo_pending=1
  local us=$'\x1f'
  _ezlo_send_raw "Q${us}${_ezlo_req_id}${us}${CURSOR}${us}${BUFFER}"
}

# Adjusts the ghost to the new buffer, as the daemon would: typing its next chars consumes them and
# deleting chars at the end gives them back. Anything else drops it until the response arrives.
_ezlo_predict_ghost() {
  local old_buf=$1
  local -i old_cur=$2
  [[ -n $_ezlo_ghost && $BUFFER != "$old_buf" ]] || return 0
  if (( old_cur == ${#old_buf} && CURSOR == ${#BUFFER} )); then
    local typed=${BUFFER:${#old_buf}}
    if [[ $BUFFER == "$old_buf"?* && $_ezlo_ghost == "$typed"* ]]; then
      _ezlo_ghost=${_ezlo_ghost:${#typed}}
      return 0
    fi
    if [[ $old_buf == "$BUFFER"?* ]]; then
      _ezlo_ghost=${old_buf:${#BUFFER}}$_ezlo_ghost
      return 0
    fi
  fi
  _ezlo_ghost=""
}

_ezlo_precmd() {
  local last_status=$?
  _ezlo_popup_open=0
  _ezlo_connect || return
  local us=$'\x1f'
  _ezlo_send_raw "STATUS${us}${last_status}"
  _ezlo_send_env
  _ezlo_send_aliases
  _ezlo_send_opts
}

_ezlo_line_finish() {
  _ezlo_popup_open=0
  _ezlo_clear_state
  _ezlo_last_buf=""
  _ezlo_last_cur=-1
  _ezlo_hist_nav=0
}

# Runs a history widget and marks the resulting buffer as a recall (see _ezlo_hist_nav).
_ezlo_hist_move() {
  _ezlo_clear_state
  (( _ezlo_req_id++ ))
  zle $1
  _ezlo_hist_nav=1
  _ezlo_hist_buf=$BUFFER
}

# Up/Down cycle none → rows → none: from no selection Down picks the first row and Up the last;
# moving past either end deselects again (Enter then runs the typed line).
_ezlo_up() {
  (( _ezlo_menu_visible )) && _ezlo_sync
  if (( _ezlo_menu_visible && ${#_ezlo_disp} > 0 )); then
    (( _ezlo_selected = _ezlo_selected == 0 ? ${#_ezlo_disp} : _ezlo_selected - 1 ))
    _ezlo_render
    zle -R
  else
    _ezlo_hist_move _ezlo_orig_up
  fi
}

_ezlo_down() {
  (( _ezlo_menu_visible )) && _ezlo_sync
  if (( _ezlo_menu_visible && ${#_ezlo_disp} > 0 )); then
    (( _ezlo_selected = _ezlo_selected < ${#_ezlo_disp} ? _ezlo_selected + 1 : 0 ))
    _ezlo_render
    zle -R
  else
    _ezlo_hist_move _ezlo_orig_down
  fi
}

# Enter on a selected row: replaces the token (or line) with it, opens `ezlo pick` for a recipe, or
# starts the translation for the "translate" row. The caller checks that a row is selected.
_ezlo_accept_selected() {
  if (( _ezlo_native )); then
    _ezlo_native_accept
  elif [[ ${_ezlo_kind[$_ezlo_selected]-} == translate ]]; then
    _ezlo_translate
  else
    local us=$'\x1f'
    local kind=${_ezlo_kind[$_ezlo_selected]-}
    # Translation suggestions aren't rows of the daemon's last completion response.
    [[ $kind == translated ]] || _ezlo_send_raw "ACCEPT${us}${_ezlo_resp_id}${us}${_ezlo_selected}"
    local ins=${_ezlo_ins[$_ezlo_selected]-}
    local suf=${_ezlo_suf[$_ezlo_selected]-}

    if [[ $kind == "recipe" ]]; then
      _ezlo_clear_state
      local picked
      picked=$(command "$_ezlo_bin" pick --recipe "$ins" --line "$BUFFER" --cursor "$CURSOR" </dev/tty)
      if [[ -n $picked ]]; then
        BUFFER=$picked
        CURSOR=${#BUFFER}
      fi
      _ezlo_last_buf=$BUFFER
      _ezlo_last_cur=$CURSOR
      zle reset-prompt
      return
    fi

    local prefix=${BUFFER[1,$_ezlo_start]}
    local rest=${BUFFER[$(( _ezlo_end + 1 )),-1]}
    [[ -n $rest && $suf == " " && $rest == " "* ]] && suf=""
    # A translation suggestion replaces the comment as its own undo step, so undo brings it back.
    [[ $kind == translated ]] && zle split-undo 2>/dev/null
    BUFFER="${prefix}${ins}${suf}${rest}"
    CURSOR=$(( ${#prefix} + ${#ins} + ${#suf} ))
    _ezlo_clear_state
    _ezlo_on_redraw
    zle -R
  fi
}

# Asks the daemon for this directory/repo's most frecent lines and shows them as the dropdown
# (picked with Up/Down and accepted with Enter like any row). Only for an empty buffer: rows
# replace the whole line.
_ezlo_recall_request() {
  if ! _ezlo_connect; then
    zle -M "ezlo: daemon unavailable"
    return
  fi
  (( _ezlo_req_id++ ))
  _ezlo_req_buf=$BUFFER
  _ezlo_last_buf=$BUFFER
  _ezlo_last_cur=$CURSOR
  _ezlo_pending=1
  local us=$'\x1f'
  _ezlo_send_raw "RECALL${us}${_ezlo_req_id}" || return
  _ezlo_sync
  (( _ezlo_menu_visible )) || zle -M "ezlo: nothing recorded for this directory yet"
  zle -R
}

# Ctrl-Space: empty-prompt workflow recall; on a non-empty buffer the key keeps its old binding.
_ezlo_recall() {
  if [[ -n $BUFFER ]] || (( ! _ezlo_enabled )); then
    zle _ezlo_orig_recall
    return
  fi
  _ezlo_recall_request
}

_ezlo_esc() {
  if [[ -n $_ezlo_tr_state ]]; then
    _ezlo_translate_cancel
    _ezlo_render
    zle -R
    return
  fi
  if (( ${#_ezlo_explain_lines} )); then
    _ezlo_explain_lines=()
    _ezlo_explain_styles=()
    _ezlo_render
    zle -R
    return
  fi
  if (( _ezlo_menu_visible )); then
    _ezlo_menu_visible=0
    _ezlo_native=0
    _ezlo_ghost=""
    _ezlo_drawn=0
    _ezlo_pane_lines=()
    _ezlo_pane_styles=()
    _ezlo_pane_spans=()
    _ezlo_pane_key=""
    _ezlo_pane_req=""
    POSTDISPLAY=""
    region_highlight=( ${region_highlight:#*memo=ezlo} )
    zle -R
  fi
}

_ezlo_right() {
  _ezlo_on_redraw
  if [[ -n $_ezlo_ghost && $CURSOR == ${#BUFFER} ]]; then
    BUFFER+=$_ezlo_ghost
    CURSOR=${#BUFFER}
    _ezlo_ghost=""
    _ezlo_on_redraw
    zle -R
  else
    zle _ezlo_orig_right
  fi
}

_ezlo_end() {
  _ezlo_on_redraw
  if [[ -n $_ezlo_ghost && $CURSOR == ${#BUFFER} ]]; then
    BUFFER+=$_ezlo_ghost
    CURSOR=${#BUFFER}
    _ezlo_ghost=""
    _ezlo_on_redraw
    zle -R
  else
    zle _ezlo_orig_end
  fi
}

_ezlo_enter() {
  if (( _ezlo_selected > 0 && _ezlo_menu_visible )) && [[ $_ezlo_tr_state != busy ]]; then
    # Catch up with typeahead first: a response for an edited line drops the selection.
    _ezlo_sync
    if (( _ezlo_selected > 0 && _ezlo_selected <= ${#_ezlo_disp} && _ezlo_menu_visible )); then
      _ezlo_accept_selected
      return
    fi
  fi
  _ezlo_clear_state
  if [[ -n $BUFFER && $BUFFER != *$'\n'* && $BUFFER != *$'\x1f'* ]]; then
    local us=$'\x1f'
    _ezlo_send_raw "EXEC${us}${PWD}${us}${BUFFER}"
  fi
  zle -R
  zle _ezlo_orig_accept_line
}

# Turns the dropdown/ghost text on or off for this shell (Ctrl-G and history recording keep working).
# Not bound by default (it is the popup's first row); bind it yourself for a one-key toggle.
_ezlo_toggle() {
  if (( _ezlo_enabled )); then
    _ezlo_enabled=0
    _ezlo_clear_state
    zle -M "ezlo: suggestions off"
  else
    _ezlo_enabled=1
    _ezlo_last_buf=""
    _ezlo_last_cur=-1
    zle -M "ezlo: suggestions on"
    _ezlo_on_redraw
  fi
  zle -R
}

# Shows/hides the selected row's preview pane for this shell (sticky until toggled again). Unbound by
# default (the Alt-E menu's `Preview pane` row does the same): `bindkey '^[p' _ezlo_pane_toggle`.
_ezlo_pane_toggle() {
  (( _ezlo_menu_visible )) && _ezlo_sync
  if (( _ezlo_pane_force > 0 )); then
    _ezlo_pane_force=-1
  elif (( _ezlo_pane_force < 0 )); then
    _ezlo_pane_force=1
  elif _ezlo_pane_wanted; then
    _ezlo_pane_force=-1
  else
    _ezlo_pane_force=1
  fi
  # The next response lays the rows out for the new mode; until then the pane may go below them.
  _ezlo_send_set
  if (( _ezlo_menu_visible )); then
    _ezlo_render
    (( _ezlo_pane_force > 0 )) && ! _ezlo_pane_wanted && zle -M "ezlo: no preview for this row"
  else
    (( _ezlo_pane_force > 0 )) && zle -M "ezlo: preview pane on" || zle -M "ezlo: preview pane off"
  fi
  zle -R
}

# Shift-Up / Shift-Down: scroll the preview pane by half its height; the key's previous widget
# when no pane is shown.
_ezlo_pane_scroll() {
  (( ${#_ezlo_pane_lines} && _ezlo_pane_view > 0 )) || return 1
  local -i step=$(( _ezlo_pane_view / 2 > 0 ? _ezlo_pane_view / 2 : 1 ))
  local -i max=$(( ${#_ezlo_pane_lines} - _ezlo_pane_view ))
  (( _ezlo_pane_off += $1 * step ))
  (( _ezlo_pane_off > max )) && _ezlo_pane_off=$max
  (( _ezlo_pane_off < 0 )) && _ezlo_pane_off=0
  _ezlo_render
  zle -R
}
_ezlo_pane_up() { _ezlo_pane_scroll -1 || zle _ezlo_orig_pane_up; }
_ezlo_pane_down() { _ezlo_pane_scroll 1 || zle _ezlo_orig_pane_down; }
# PageUp / PageDown: the same, for terminals (or tmux setups) that don't send Shift-arrows.
_ezlo_pane_pgup() { _ezlo_pane_scroll -1 || zle _ezlo_orig_pane_pgup; }
_ezlo_pane_pgdown() { _ezlo_pane_scroll 1 || zle _ezlo_orig_pane_pgdown; }

# Alt-E: opens the settings popup and runs it as a modal recursive edit in the `ezlo-popup` keymap
# (keys not bound there are swallowed, so the line can't change underneath it) until Esc/q/Alt-E.
_ezlo_popup() {
  _ezlo_explain_lines=()
  _ezlo_explain_styles=()
  _ezlo_popup_open=1
  _ezlo_popup_sel=1
  # Fetch the daemon's config values once per connection, so the popup shows the real defaults.
  if (( ! ${#_ezlo_cfg} )) && _ezlo_connect && _ezlo_send_set; then
    local line
    while (( _ezlo_fd >= 0 && ! ${#_ezlo_cfg} )) && _ezlo_getline 0.5; do
      _ezlo_consume $_ezlo_fd "$REPLY"
    done
  fi
  _ezlo_render
  zle -R
  zle recursive-edit -K ezlo-popup
  _ezlo_popup_open=0
  _ezlo_clear_state
  _ezlo_last_buf=""
  _ezlo_last_cur=-1
  _ezlo_on_redraw
  zle -R
}

_ezlo_popup_up() {
  (( _ezlo_popup_sel = _ezlo_popup_sel > 1 ? _ezlo_popup_sel - 1 : ${#_ezlo_popup_keys} ))
  _ezlo_render
  zle -R
}

_ezlo_popup_down() {
  (( _ezlo_popup_sel = _ezlo_popup_sel < ${#_ezlo_popup_keys} ? _ezlo_popup_sel + 1 : 1 ))
  _ezlo_render
  zle -R
}

_ezlo_popup_inc() { _ezlo_popup_change ${_ezlo_popup_keys[$_ezlo_popup_sel]} 1; _ezlo_render; zle -R; }
_ezlo_popup_dec() { _ezlo_popup_change ${_ezlo_popup_keys[$_ezlo_popup_sel]} -1; _ezlo_render; zle -R; }
_ezlo_popup_activate() { _ezlo_popup_change ${_ezlo_popup_keys[$_ezlo_popup_sel]} 0; _ezlo_render; zle -R; }
# Ends the popup's recursive edit (accept-line returns from it; the line itself isn't accepted).
_ezlo_popup_close() { zle .accept-line; }
_ezlo_popup_ignore() { : }

_ezlo_pick_widget() {
  _ezlo_clear_state
  zle -R
  local picked
  picked=$(command "$_ezlo_bin" pick --line "$BUFFER" --cursor "$CURSOR" </dev/tty)
  if [[ -n $picked ]]; then
    BUFFER=$picked
    CURSOR=${#BUFFER}
  fi
  _ezlo_last_buf=$BUFFER
  _ezlo_last_cur=$CURSOR
  zle reset-prompt
}

# Ctrl-R: `ezlo history search` seeded with the current line; the picked line replaces the buffer
# and stays quiet like an Up-recalled line until edited, or runs right away when picked with Ctrl-O
# (exit status 3). Esc keeps the buffer. If ezlo itself fails (not an abort), the key's previous
# widget runs instead.
_ezlo_search() {
  _ezlo_clear_state
  zle -R
  local picked
  local -a args=( history search --query "$BUFFER" )
  [[ -n ${HISTFILE-} ]] && args+=( --histfile "$HISTFILE" )
  [[ -o hist_ignore_space ]] && args+=( --ignore-space )
  picked=$(command "$_ezlo_bin" "${args[@]}" </dev/tty)
  local -i rc=$?
  if (( rc == 3 )) && [[ -n $picked ]]; then
    BUFFER=$picked
    CURSOR=${#BUFFER}
    zle _ezlo_enter
    return
  fi
  if (( rc != 0 && rc != 130 )); then
    zle reset-prompt
    zle _ezlo_orig_search
    return
  fi
  if [[ -n $picked ]]; then
    BUFFER=$picked
    CURSOR=${#BUFFER}
    _ezlo_hist_nav=1
    _ezlo_hist_buf=$BUFFER
  fi
  _ezlo_last_buf=$BUFFER
  _ezlo_last_cur=$CURSOR
  zle reset-prompt
}

# Alt-H: explains the current line (bindings, diagnostics, candidate provenance) below the prompt,
# asking the daemon (session aliases, native fallback) and falling back to the one-shot CLI.
_ezlo_explain() {
  if [[ -z ${BUFFER// } || $BUFFER == *$'\n'* || $BUFFER == *$'\x1f'* ]]; then
    zle -M "ezlo: nothing to explain"
    return
  fi
  _ezlo_explain_lines=()
  _ezlo_explain_styles=()
  _ezlo_explain_buf=$BUFFER
  _ezlo_explain_cur=$CURSOR
  # Treat the line as seen so no completion query races the explanation.
  _ezlo_last_buf=$BUFFER
  _ezlo_last_cur=$CURSOR
  local us=$'\x1f' line
  if _ezlo_connect; then
    (( _ezlo_req_id++ ))
    _ezlo_pending=0
    local -i rid=$_ezlo_req_id
    if _ezlo_send_raw "EXPLAIN${us}${rid}${us}${CURSOR}${us}${BUFFER}"; then
      while (( _ezlo_fd >= 0 && _ezlo_explain_id != rid )) && _ezlo_getline 2; do
        _ezlo_consume $_ezlo_fd "$REPLY"
      done
    fi
  fi
  if (( ! ${#_ezlo_explain_lines} )); then
    local out
    out=$(command "$_ezlo_bin" explain --line "$BUFFER" --cursor "$CURSOR" 2>/dev/null)
    _ezlo_explain_lines=( "${(@f)out}" )
    _ezlo_explain_styles=( "bold" )
    [[ -n $out ]] || _ezlo_explain_lines=( "ezlo: explain unavailable" )
  fi
  _ezlo_menu_visible=0
  _ezlo_render
  zle -R
}

# Ctrl-X Ctrl-L (and Enter on the selected "translate" row of a comment line): asks the daemon to
# turn the line (a `# comment`, or any text) into commands with the `[complete.translate]` command.
# Runs in the background: a status line shows until the suggestions arrive as dropdown rows (Enter
# on a row picked with Up/Down replaces the line with it, undo restores it; Esc or editing
# dismisses them) or the error
# replaces it; Esc or editing the line cancels.
_ezlo_translate() {
  if [[ -z ${BUFFER//[[:space:]\#]/} || $BUFFER == *$'\x1f'* ]]; then
    zle -M "ezlo: nothing to translate"
    return
  fi
  if ! _ezlo_connect; then
    zle -M "ezlo: daemon unavailable"
    return
  fi
  _ezlo_clear_state
  (( _ezlo_req_id++ ))
  _ezlo_tr_id=$_ezlo_req_id
  _ezlo_tr_buf=$BUFFER
  _ezlo_tr_state=busy
  # Treat the line as seen so no completion query races the translation.
  _ezlo_last_buf=$BUFFER
  _ezlo_last_cur=$CURSOR
  local us=$'\x1f'
  if ! _ezlo_send_raw "TRANSLATE${us}${_ezlo_tr_id}${us}${BUFFER//$'\n'/ }"; then
    zle -M "ezlo: lost the daemon connection, translation not sent"
    return
  fi
  _ezlo_render
  zle -R
}

# Fallback widgets: reuse whatever the key was bound to before ezlo (e.g. zsh-abbr on Enter),
# falling back to the builtin when unbound or already a ezlo widget.
_ezlo_capture_orig() {
  local orig=$1 key=$2 builtin=$3 w
  w=${${(z)"$(bindkey -M main -- "$key" 2>/dev/null)"}[2]}
  if [[ -n $w && $w != undefined-key && $w != _ezlo_* && -n ${widgets[$w]} ]] && zle -A "$w" "$orig" 2>/dev/null; then
    return
  fi
  zle -A "$builtin" "$orig" 2>/dev/null || zle -N "$orig" "$builtin"
}

zle -N _ezlo_fd_handler
zle -N _ezlo_up
zle -N _ezlo_down
zle -N _ezlo_tab
zle -N _ezlo_stab
zle -C _ezlo_native_apply .complete-word _ezlo_native_apply_fn
zle -N _ezlo_esc
zle -N _ezlo_right
zle -N _ezlo_end
zle -N _ezlo_enter
zle -N _ezlo_pick_widget
zle -N _ezlo_search
zle -N _ezlo_toggle
zle -N _ezlo_explain
zle -N _ezlo_recall
zle -N _ezlo_pane_toggle
zle -N _ezlo_pane_up
zle -N _ezlo_pane_down
zle -N _ezlo_pane_pgup
zle -N _ezlo_pane_pgdown
zle -N _ezlo_translate
zle -N _ezlo_popup
zle -N _ezlo_popup_up
zle -N _ezlo_popup_down
zle -N _ezlo_popup_inc
zle -N _ezlo_popup_dec
zle -N _ezlo_popup_activate
zle -N _ezlo_popup_close
zle -N _ezlo_popup_ignore

_ezlo_bind_keys() {
  local key_up key_down key_tab key_stab key_esc key_right key_end key_pick key_popup key_explain key_recall key_pane_up key_pane_down key_pane_pgup key_pane_pgdown key_translate key_search
  zstyle -s ':ezlo:keys' up key_up || key_up='^[[A'
  zstyle -s ':ezlo:keys' down key_down || key_down='^[[B'
  # Tab shows zsh's completion in the dropdown ('' keeps zsh's own Tab untouched).
  zstyle -s ':ezlo:keys' complete key_tab || key_tab='^I'
  zstyle -s ':ezlo:keys' complete-prev key_stab || key_stab='^[[Z'
  zstyle -s ':ezlo:keys' cancel key_esc || key_esc='^['
  zstyle -s ':ezlo:keys' ghost-right key_right || key_right='^[[C'
  zstyle -s ':ezlo:keys' ghost-end key_end || key_end='^E'
  zstyle -s ':ezlo:keys' pick key_pick || key_pick='^G'
  # `toggle` is the pre-popup name of the same key.
  zstyle -s ':ezlo:keys' popup key_popup || zstyle -s ':ezlo:keys' toggle key_popup || key_popup='^[e'
  zstyle -s ':ezlo:keys' explain key_explain || key_explain='^[h'
  zstyle -s ':ezlo:keys' recall key_recall || key_recall='^@'
  zstyle -s ':ezlo:keys' pane-up key_pane_up || key_pane_up='^[[1;2A'
  zstyle -s ':ezlo:keys' pane-down key_pane_down || key_pane_down='^[[1;2B'
  zstyle -s ':ezlo:keys' pane-page-up key_pane_pgup || key_pane_pgup='^[[5~'
  zstyle -s ':ezlo:keys' pane-page-down key_pane_pgdown || key_pane_pgdown='^[[6~'
  zstyle -s ':ezlo:keys' translate key_translate || key_translate='^X^L'
  zstyle -s ':ezlo:keys' search key_search || key_search='^R'

  zmodload zsh/zleparameter 2>/dev/null
  _ezlo_capture_orig _ezlo_orig_up "$key_up" up-line-or-history
  _ezlo_capture_orig _ezlo_orig_down "$key_down" down-line-or-history
  [[ -n $key_tab ]] && _ezlo_capture_orig _ezlo_orig_tab "$key_tab" expand-or-complete
  [[ -n $key_stab ]] && _ezlo_capture_orig _ezlo_orig_stab "$key_stab" beep
  _ezlo_capture_orig _ezlo_orig_right "$key_right" forward-char
  _ezlo_capture_orig _ezlo_orig_end "$key_end" end-of-line
  _ezlo_capture_orig _ezlo_orig_accept_line '^M' accept-line
  _ezlo_capture_orig _ezlo_orig_recall "$key_recall" set-mark-command
  _ezlo_capture_orig _ezlo_orig_search "$key_search" history-incremental-search-backward
  _ezlo_capture_orig _ezlo_orig_pane_up "$key_pane_up" beep
  _ezlo_capture_orig _ezlo_orig_pane_down "$key_pane_down" beep
  _ezlo_capture_orig _ezlo_orig_pane_pgup "$key_pane_pgup" beep
  _ezlo_capture_orig _ezlo_orig_pane_pgdown "$key_pane_pgdown" beep

  local km
  for km in emacs viins; do
    bindkey -M "$km" "$key_up" _ezlo_up
    bindkey -M "$km" '^[OA' _ezlo_up
    bindkey -M "$km" "$key_down" _ezlo_down
    bindkey -M "$km" '^[OB' _ezlo_down
    [[ -n $key_tab ]] && bindkey -M "$km" "$key_tab" _ezlo_tab
    [[ -n $key_stab ]] && bindkey -M "$km" "$key_stab" _ezlo_stab
    bindkey -M "$km" "$key_esc" _ezlo_esc
    bindkey -M "$km" "$key_right" _ezlo_right
    bindkey -M "$km" '^[OC' _ezlo_right
    bindkey -M "$km" "$key_end" _ezlo_end
    bindkey -M "$km" '^[[F' _ezlo_end
    bindkey -M "$km" '^[OF' _ezlo_end
    bindkey -M "$km" '^M' _ezlo_enter
    bindkey -M "$km" '^J' _ezlo_enter
    bindkey -M "$km" "$key_pick" _ezlo_pick_widget
    bindkey -M "$km" "$key_popup" _ezlo_popup
    bindkey -M "$km" "$key_explain" _ezlo_explain
    bindkey -M "$km" "$key_recall" _ezlo_recall
    bindkey -M "$km" "$key_pane_up" _ezlo_pane_up
    bindkey -M "$km" "$key_pane_down" _ezlo_pane_down
    bindkey -M "$km" "$key_pane_pgup" _ezlo_pane_pgup
    bindkey -M "$km" "$key_pane_pgdown" _ezlo_pane_pgdown
    bindkey -M "$km" "$key_translate" _ezlo_translate
    [[ -n $key_search ]] && bindkey -M "$km" "$key_search" _ezlo_search
  done
  bindkey -M vicmd "$key_pick" _ezlo_pick_widget 2>/dev/null || true
  bindkey -M vicmd "$key_translate" _ezlo_translate 2>/dev/null || true
  [[ -n $key_search ]] && { bindkey -M vicmd "$key_search" _ezlo_search 2>/dev/null || true; }

  # The popup's own keymap: everything printable is swallowed, then the popup keys.
  bindkey -N ezlo-popup
  bindkey -M ezlo-popup -R -- ' '-'~' _ezlo_popup_ignore
  local k
  for k in '^[[A' '^[OA' k '^P'; do bindkey -M ezlo-popup -- "$k" _ezlo_popup_up; done
  for k in '^[[B' '^[OB' j '^N'; do bindkey -M ezlo-popup -- "$k" _ezlo_popup_down; done
  for k in '^[[C' '^[OC' l + =; do bindkey -M ezlo-popup -- "$k" _ezlo_popup_inc; done
  for k in '^[[D' '^[OD' h -; do bindkey -M ezlo-popup -- "$k" _ezlo_popup_dec; done
  for k in ' ' '^M' '^J'; do bindkey -M ezlo-popup -- "$k" _ezlo_popup_activate; done
  for k in '^[' q '^G' "$key_popup"; do bindkey -M ezlo-popup -- "$k" _ezlo_popup_close; done
}

_ezlo_bind_keys
add-zle-hook-widget line-pre-redraw _ezlo_on_redraw
add-zle-hook-widget line-finish _ezlo_line_finish
add-zsh-hook precmd _ezlo_precmd
TRAPWINCH() { _ezlo_send_opts; }
