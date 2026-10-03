"""mise usage specs generated from cli.py's argparse definitions, and back.

mise parses a task's arguments against its `usage` spec (giving real --help,
choices and completion) and passes the values as `usage_<name>` variables.
`argv` rebuilds the argparse command line from them. Arguments after `--`
bypass the spec and arrive as raw argv instead.
"""
import argparse
import re
import shlex

from runtime import ROOT

MISE = ROOT / 'mise.toml'


def _kdl(text):
    return '"' + str(text).replace('\\', '\\\\').replace('"', '\\"') + '"'


def _actions(parser):
    return [a for a in parser._actions if not isinstance(a, argparse._HelpAction)]


def _subparsers(parser):
    return next((a for a in parser._actions if isinstance(a, argparse._SubParsersAction)), None)


def _flag(action):
    return max(action.option_strings, key=len)


def _spec(parser, indent=''):
    lines = []
    for action in _actions(parser):
        if isinstance(action, argparse._SubParsersAction):
            helps = {choice.dest: choice.help for choice in action._choices_actions}
            for name, sub in action.choices.items():
                body = _spec(sub, indent + '  ')
                head = f'{indent}cmd {_kdl(name)}' + (f' help={_kdl(helps[name])}' if helps.get(name) else '')
                lines.append(head + (' {\n' + body + '\n' + indent + '}' if body else ''))
            continue
        attrs = []
        if not action.option_strings:
            name = action.dest
            if action.nargs == argparse.REMAINDER:
                head = f'arg {_kdl(f"[{name}]...")}'
                attrs.append('var=#true')
            elif action.nargs == '?':
                head = f'arg {_kdl(f"[{name}]")}'
            else:
                head = f'arg {_kdl(f"<{name}>")}'
        elif isinstance(action, argparse._StoreTrueAction):
            head = f'flag {_kdl(" ".join(action.option_strings))}'
        else:
            metavar = _flag(action).lstrip('-')
            head = f'flag {_kdl(" ".join(action.option_strings) + f" <{metavar}>")}'
            if isinstance(action, argparse._AppendAction):
                attrs.append('var=#true')
        if action.help:
            attrs.append(f'help={_kdl(action.help)}')
        if action.default not in (None, False, [], argparse.SUPPRESS):
            attrs.append(f'default={_kdl(action.default)}')
        line = indent + ' '.join([head, *attrs])
        if action.choices:
            line += ' {\n' + indent + '  choices ' + ' '.join(map(_kdl, action.choices)) + '\n' + indent + '}'
        lines.append(line)
    return '\n'.join(lines)


def specs(top):
    return {name: _spec(sub) for name, sub in _subparsers(top).choices.items()}


def argv(top, command, env):
    """The argparse argv for a mise task run, from its usage_* variables."""
    path = env.get('usage_cmd', '').split()

    def build(parser, path):
        positional, options, remainder, tail = [], [], [], []
        for action in _actions(parser):
            if isinstance(action, argparse._SubParsersAction):
                if path:
                    tail = [path[0], *build(action.choices[path[0]], path[1:])]
                continue
            key = 'usage_' + (_flag(action).lstrip('-') if action.option_strings else action.dest).replace('-', '_')
            value = env.get(key)
            if value is None:
                continue
            if not action.option_strings:
                if action.nargs == argparse.REMAINDER:
                    remainder = shlex.split(value)
                else:
                    positional.append(value)
            elif isinstance(action, argparse._StoreTrueAction):
                options += [_flag(action)] if value == 'true' else []
            elif isinstance(action, argparse._AppendAction):
                for item in shlex.split(value):
                    options += [_flag(action), item]
            else:
                options += [_flag(action), value]
        return positional + options + tail + remainder

    return [command, *build(_subparsers(top).choices[command], path)]


def render(top, text):
    """mise.toml text with each task's usage replaced by the generated spec."""
    generated = specs(top)
    def task(match):
        name, block = match.group(1), match.group(0)
        block = re.sub(r"usage = '''\n.*?'''\n", '', block, flags=re.S)
        if generated.get(name):
            block = re.sub(r'(description = .*\n)', lambda m: m.group(1) + "usage = '''\n" + generated[name] + "\n'''\n", block, count=1)
        return block
    return re.sub(r'^\[tasks\.([a-z-]+)\]\n(?:(?!^\[).*\n?)*', task, text, flags=re.M)


def write(top):
    text = MISE.read_text()
    updated = render(top, text)
    if updated != text:
        MISE.write_text(updated)
        print('Updated mise.toml task usage from cli.py')
