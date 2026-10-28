"""Guest CLI for brokered PR/issue operations; outputs JSON and holds no GitHub token."""
import argparse
from pathlib import Path
import sys
from github_common import Failure, repository


def parse(args):
    parser = argparse.ArgumentParser(prog='kuggis-gh --repo OWNER/REPO --', allow_abbrev=False,
                                     description=__doc__)
    groups = parser.add_subparsers(dest='kind', required=True)
    for kind in ('pr', 'issue'):
        actions = groups.add_parser(kind, allow_abbrev=False).add_subparsers(dest='action', required=True)
        names = ['list', 'view', 'create', 'edit', 'close', 'reopen', 'comment', 'comments']
        if kind == 'pr':
            names += ['diff', 'reviews', 'review', 'merge']
        for action in names:
            command = actions.add_parser(action, allow_abbrev=False)
            if action not in ('list', 'create'):
                command.add_argument('number', type=int)
            if action in ('list', 'comments', 'diff', 'reviews'):
                command.add_argument('--limit', type=int, default=30)
                command.add_argument('--page', type=int, default=1)
            if action == 'list':
                command.add_argument('--state', choices=('open', 'closed', 'all'), default='open')
            if action in ('create', 'edit'):
                command.add_argument('--title', required=action == 'create')
            if action in ('create', 'edit', 'comment', 'review'):
                text = command.add_mutually_exclusive_group(required=action == 'comment')
                text.add_argument('--body')
                text.add_argument('--body-file')
            if kind == 'pr' and action == 'create':
                command.add_argument('--base', required=True)
                command.add_argument('--head', required=True)
                command.add_argument('--head-repo')
                command.add_argument('--draft', action='store_true')
            if action == 'review':
                events = command.add_mutually_exclusive_group(required=True)
                for flag, event in [('approve', 'APPROVE'), ('request-changes', 'REQUEST_CHANGES'), ('comment', 'COMMENT')]:
                    events.add_argument('--' + flag, dest='event', action='store_const', const=event)
            if action == 'merge':
                command.add_argument('--method', choices=('merge', 'squash', 'rebase'), default='merge')
                command.add_argument('--match-head-commit', dest='sha', required=True)
    value = vars(parser.parse_args(args))
    kind, action = value.pop('kind'), value.pop('action')
    body_file = value.pop('body_file', None)
    if body_file is not None:
        if body_file == '-':
            body = sys.stdin.read(30001)
        else:
            with Path(body_file).open() as stream:
                body = stream.read(30001)
        if len(body) > 30000:
            raise Failure('body file exceeds 30000 characters')
        value['body'] = body
    return {'kind': kind, 'action': action, 'params': {k: v for k, v in value.items() if v is not None}}


def run(repo, args, send):
    body = {'repo': repository(repo), **parse(args)}
    return send('/api', body)
