"""Host firewall comparison must ignore chain creation order, not rule order."""
import copy
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

source = Path(__file__).resolve().parents[2] / 'cell/src/environment/host.py'
spec = importlib.util.spec_from_file_location('production_environment', source)
host = importlib.util.module_from_spec(spec)
with patch.object(Path, 'read_text', return_value=json.dumps({
        'machine': 'cell-env', 'interface': 've-cell-env', 'host_ip': '10.203.0.1',
        'guest_ip': '10.203.0.2', 'uid_base': 524288, 'uid_count': 65536})):
    spec.loader.exec_module(host)


class PolicyTests(unittest.TestCase):
    def setUp(self):
        self.rules = {'nftables': [
            {'chain': {'name': 'out', 'hook': 'output', 'prio': 0}},
            {'rule': {'chain': 'out', 'expr': [{'reject': None}], 'handle': 3}},
            {'chain': {'name': 'input', 'hook': 'input', 'prio': -200}},
            {'rule': {'chain': 'input', 'expr': [{'accept': None}], 'handle': 5}},
            {'rule': {'chain': 'input', 'expr': [{'counter': {'packets': 5}}, {'reject': None}], 'handle': 6}},
        ]}

    def test_reboot_chain_order_and_counters_do_not_change_policy(self):
        after = copy.deepcopy(self.rules)
        after['nftables'] = after['nftables'][2:] + after['nftables'][:2]
        after['nftables'][2]['rule']['expr'][0]['counter']['packets'] = 100
        self.assertEqual(host.policy(self.rules), host.policy(after))

    def test_reordered_rules_and_changed_hook_are_rejected(self):
        changed = copy.deepcopy(self.rules)
        changed['nftables'][3:5] = reversed(changed['nftables'][3:5])
        self.assertNotEqual(host.policy(self.rules), host.policy(changed))
        changed = copy.deepcopy(self.rules)
        changed['nftables'][2]['chain']['prio'] = 100
        self.assertNotEqual(host.policy(self.rules), host.policy(changed))


if __name__ == '__main__':
    unittest.main()
