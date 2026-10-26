"""Synthetic policy checks. No SSH, systemd mutations, or real service reads."""
import importlib.util
from pathlib import Path
import os
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('host_agents', HERE.parent / 'src/host_agents.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


def row(state='inactive', mask='masked', pid='0'):
    return {'ActiveState': state, 'UnitFileState': mask, 'MainPID': pid,
            'Requires': '', 'BindsTo': '', 'PartOf': '', 'LoadState': 'loaded'}


class PolicyTests(unittest.TestCase):
    def test_disabled_active_or_runtime_mask_is_not_safe(self):
        for bad in (row('active', 'disabled'), row('inactive', 'disabled'),
                    row('active', 'masked'), row('inactive', 'masked-runtime'), row(pid='42')):
            self.assertFalse(policy.stopped_and_masked({'shelley.service': bad}))
        self.assertTrue(policy.stopped_and_masked({'shelley.service': row()}))

    def test_dependents_prevent_any_mutation(self):
        dependent = row('active', pid='19')
        dependent['Requires'] = 'shelley.service'
        with patch.object(policy, 'states', return_value={'cell-agent.service': dependent}), \
                patch.object(policy, 'command') as command:
            with self.assertRaises(RuntimeError):
                policy.apply_policy({})
            command.assert_not_called()

    def test_unexpected_managed_restart_is_reported(self):
        before = {'cell-agent.service': row('active', pid='100')}
        after = {'cell-agent.service': row('active', pid='101')}
        report = {}
        with patch.object(policy, 'states', side_effect=[before, {u: row('active', 'enabled') for u in policy.AGENTS},
                                                       {u: row() for u in policy.AGENTS}, after]), \
                patch.object(policy, 'command') as command, \
                patch.object(policy, 'install_masks', return_value=([], None)):
            with self.assertRaises(RuntimeError):
                policy.apply_policy(report)
            self.assertFalse(report['managed_services_unchanged'])
            for call in command.call_args_list:
                self.assertFalse(set(call.args).intersection(policy.MANAGED))
                self.assertNotIn('restart', call.args)

    def test_backup_and_repeat_preserve_local_units(self):
        with tempfile.TemporaryDirectory(dir=HERE) as directory:
            root = Path(directory)
            units, backups = root / 'units', root / 'backups'
            units.mkdir(mode=0o755)
            (units / 'shelley.service').write_text('synthetic local unit definition')
            (units / 'shelley.socket').symlink_to('/synthetic/vendor/socket')
            original = Path.lstat
            def root_owned(path):
                info = original(path)
                fields = list(info)
                fields[4] = 0  # emulate root ownership, retaining real file types
                return os.stat_result(fields)
            def disable(*args):
                self.assertEqual(args, ('disable', *policy.AGENTS))
                # systemctl disable may remove a symlink unit definition.
                (units / 'shelley.socket').unlink()
            with patch.object(Path, 'lstat', root_owned), patch.object(policy, 'command', disable):
                changes, batch = policy.install_masks(units, backups, disable=policy.AGENTS)
                self.assertEqual((batch / 'shelley.service').read_text(), 'synthetic local unit definition')
                self.assertEqual(os.readlink(batch / 'shelley.socket'), '/synthetic/vendor/socket')
                self.assertTrue(all(os.readlink(units / name) == '/dev/null' for name in policy.AGENTS))
                again, repeat_batch = policy.install_masks(units, backups)
                self.assertIsNone(repeat_batch)
                self.assertTrue(all(item['already_masked'] for item in again))
                self.assertEqual(len(list(backups.iterdir())), 1)

    def test_symlink_unit_directory_is_rejected(self):
        with tempfile.TemporaryDirectory(dir=HERE) as directory:
            root = Path(directory)
            (root / 'real').mkdir()
            (root / 'units').symlink_to(root / 'real')
            with self.assertRaises(RuntimeError):
                policy.install_masks(root / 'units', root / 'backups')
            self.assertEqual(list((root / 'real').iterdir()), [])

    def test_incomplete_systemd_metadata_is_rejected(self):
        with patch.object(policy, 'command', return_value='Id=shelley.service\nActiveState=inactive\n'):
            with self.assertRaises(RuntimeError):
                policy.states(policy.AGENTS)

    def test_missing_properties_fail_closed_even_when_both_units_are_present(self):
        rows = {u: dict(row(), Id=u, SubState='dead') for u in policy.AGENTS}
        def output():
            return '\n\n'.join('\n'.join(f'{k}={v}' for k, v in r.items()) for r in rows.values())
        for unit in policy.AGENTS:
            for key in policy.PROPERTIES:
                if key == 'MainPID' and unit.endswith('.socket'):
                    continue
                saved = rows[unit].pop(key)
                with self.subTest(unit=unit, key=key), patch.object(policy, 'command', return_value=output()):
                    with self.assertRaises(RuntimeError):
                        policy.states(policy.AGENTS)
                rows[unit][key] = saved
        # systemd sockets do not have MainPID; empty dependency values are valid.
        rows['shelley.socket'].pop('MainPID')
        with patch.object(policy, 'command', return_value=output()):
            self.assertTrue(policy.stopped_and_masked(policy.states(policy.AGENTS)))

    def test_fresh_image_without_shelley_still_gets_persistent_masks(self):
        managed = {name: row() for name in policy.MANAGED}
        missing = {name: dict(row(mask=''), LoadState='not-found') for name in policy.AGENTS}
        after = {name: row() for name in policy.AGENTS}
        report = {}
        with patch.object(policy, 'states', side_effect=[managed, missing, after, managed]), \
                patch.object(policy, 'install_masks', return_value=([], None)) as masks, \
                patch.object(policy, 'command') as command:
            policy.apply_policy(report)
            masks.assert_called_once_with(disable=[])
            command.assert_called_once_with('daemon-reload')
        self.assertTrue(report['stopped_and_masked'])
        self.assertTrue(report['managed_services_unchanged'])

    def test_policy_failure_never_reports_success(self):
        managed = {name: row() for name in policy.MANAGED}
        unsafe = {name: row('active', 'enabled', '123') for name in policy.AGENTS}
        report = {}
        with patch.object(policy, 'states', side_effect=[managed, unsafe, unsafe, managed]), \
                patch.object(policy, 'install_masks', return_value=([], None)), \
                patch.object(policy, 'command'):
            with self.assertRaisesRegex(RuntimeError, 'not fully stopped'):
                policy.apply_policy(report)
        self.assertFalse(report['stopped_and_masked'])


if __name__ == '__main__':
    unittest.main()
