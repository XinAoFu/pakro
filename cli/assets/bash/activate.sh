# shellcheck shell=bash
export __PAKRO_EXE=__PAKRO_EXE_VALUE__
__PAKRO_FLAGS=(__PAKRO_FLAGS_VALUE__)
__PAKRO_HOOK_ENABLED=__PAKRO_HOOK_ENABLED_VALUE__

export PAKRO_SHELL=bash

# On first activation, save the original PATH
# On re-activation, we keep the saved original
if [ -z "${__PAKRO_ORIG_PATH:-}" ]; then
	export __PAKRO_ORIG_PATH="$PATH"
fi
__PAKRO_BASH_CHPWD_RAN=0

pakro() {
	local command
	command="${1:-}"
	if [ "$#" = 0 ]; then
		command __PAKRO_EXE_VALUE__
		return
	fi
	shift

	case "$command" in
	deactivate | shell | sh)
		# if argv doesn't contains -h,--help
		if [[ ! " $* " =~ " --help " ]] && [[ ! " $* " =~ " -h " ]]; then
			eval "$(command __PAKRO_EXE_VALUE__ "$command" "$@")"
			return $?
		fi
		;;
	esac
	command __PAKRO_EXE_VALUE__ "$command" "$@"
}

_pakro_hook() {
	local previous_exit_status=$?
	eval "$(pakro hook-env ${__PAKRO_FLAGS[@]+"${__PAKRO_FLAGS[@]}"} --shell-pid $$ -s bash "$@")"
	return $previous_exit_status
}

if [ "$__PAKRO_HOOK_ENABLED" = "1" ]; then
	_pakro_hook_prompt_command() {
		local previous_exit_status=$?
		if [[ ${__PAKRO_BASH_CHPWD_RAN:-0} == "1" ]]; then
			__PAKRO_BASH_CHPWD_RAN=0
			unset __PAKRO_BASH_SKIP_FIRST_PROMPT
			return $previous_exit_status
		fi
		if [[ ${__PAKRO_BASH_SKIP_FIRST_PROMPT:-0} == "1" ]]; then
			unset __PAKRO_BASH_SKIP_FIRST_PROMPT
			return $previous_exit_status
		fi
		eval "$(pakro hook-env ${__PAKRO_FLAGS[@]+"${__PAKRO_FLAGS[@]}"} --shell-pid $$ -s bash --reason precmd)"
		return $previous_exit_status
	}

	_pakro_hook_chpwd() {
		local previous_exit_status=$?
		__PAKRO_BASH_CHPWD_RAN=1
		eval "$(pakro hook-env ${__PAKRO_FLAGS[@]+"${__PAKRO_FLAGS[@]}"} --shell-pid $$ -s bash --reason chpwd)"
		return $previous_exit_status
	}

	_pakro_add_prompt_command() {
		if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
			if [[ " ${PROMPT_COMMAND[*]} " != *" _pakro_hook_prompt_command "* ]]; then
				PROMPT_COMMAND=("_pakro_hook_prompt_command" "${PROMPT_COMMAND[@]}")
			fi
		elif [[ ";${PROMPT_COMMAND:-};" != *";_pakro_hook_prompt_command;"* ]]; then
			local _pakro_prompt_command_value="${PROMPT_COMMAND-}"
			printf -v PROMPT_COMMAND '%s' "_pakro_hook_prompt_command${_pakro_prompt_command_value:+;$_pakro_prompt_command_value}"
		fi
	}

	_pakro_add_prompt_command
	__PAKRO_CHPWD_FUNCTIONS__
	__PAKRO_CHPWD_LOAD__
	chpwd_functions+=(_pakro_hook_chpwd)
fi

# `--no-hook-env` means pakro does not apply the environment at activation -- zsh, fish and pwsh
# all keep their equivalent call inside this guard, and bash did too until the script moved out
# of bash.rs into this file (#8920), which left the call outside it. The skip flag exists because
# the hook runs here, so the two belong under one condition; it is set first so the `$?` that
# `_pakro_hook` saves is not read straight off the guard's condition, where it is the test's
# status rather than a command's (SC2319).
if [ "$__PAKRO_HOOK_ENABLED" = "1" ]; then
	__PAKRO_BASH_SKIP_FIRST_PROMPT=1
	_pakro_hook --force
fi
