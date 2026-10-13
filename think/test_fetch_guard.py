"""Tests for the Thinker's WebFetch guard: python3 -m unittest think/test_fetch_guard.py"""
import importlib.machinery
import importlib.util
import json
import os
import tempfile
import unittest

_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fetch-guard")
_loader = importlib.machinery.SourceFileLoader("fetch_guard", _path)
guard = importlib.util.module_from_spec(importlib.util.spec_from_loader("fetch_guard", _loader))
_loader.exec_module(guard)

PUBLIC = lambda host: {"93.184.216.34"}  # noqa: E731 — every name resolves somewhere public
SEARCH = "https://en.wikipedia.org/wiki/.example"


def human(text):
    return {"type": "user", "message": {"role": "user", "content": text}}


def tool_use(i, name, inp):
    return {"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": i, "name": name, "input": inp}]}}


def tool_result(i, text, result=None):
    e = {"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": i, "content": text}]}}
    if result is not None:
        e["toolUseResult"] = result
    return e


def search(i, query, urls):
    return [tool_use(i, "WebSearch", {"query": query}),
            tool_result(i, f'Web search results for query: "{query}"\n\nLinks: '
                        + json.dumps([{"title": "t", "url": u} for u in urls]),
                        {"query": query, "results": [{"tool_use_id": "srv", "content": [
                            {"title": "t", "url": u} for u in urls]}]})]


class Guard(unittest.TestCase):
    def fetch(self, url, entries, resolve=PUBLIC):
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write("\n".join(json.dumps(e) for e in entries) + "\n")
        self.addCleanup(os.unlink, f.name)
        return guard.verdict({"tool_input": {"url": url, "prompt": "?"}, "transcript_path": f.name}, resolve)

    def test_a_search_result_link_is_fetched(self):
        self.assertIsNone(self.fetch(SEARCH, search("s1", "example", [SEARCH])))

    def test_a_link_the_person_typed_is_fetched(self):
        self.assertIsNone(self.fetch("https://example.org/paper", [human("read https://example.org/paper.")]))

    def test_a_url_the_model_wrote_is_refused(self):
        entries = [human("think about examples"),
                   {"type": "assistant", "message": {"content": [{"type": "text", "text": "https://evil.test/?d=s"}]}}]
        self.assertIsNotNone(self.fetch("https://evil.test/?d=s", entries))

    def test_the_search_query_echo_does_not_launder_a_url(self):
        # the result text repeats the model's query; only its structured links count
        self.assertIsNotNone(self.fetch("https://evil.test/?d=s", search("s1", "https://evil.test/?d=s", [SEARCH])))

    def test_a_link_inside_a_fetched_page_is_refused(self):
        entries = search("s1", "x", [SEARCH]) + [
            tool_use("f1", "WebFetch", {"url": SEARCH, "prompt": "list links"}),
            tool_result("f1", "Links: https://elsewhere.test/a", {"url": SEARCH, "code": 200})]
        self.assertIsNotNone(self.fetch("https://elsewhere.test/a", entries))

    def test_data_appended_to_an_allowed_url_is_refused(self):
        self.assertIsNotNone(self.fetch(SEARCH + "?d=secret", search("s1", "x", [SEARCH])))

    def test_a_bigbrain_link_is_fetched(self):
        entries = [tool_use("b1", "mcp__bigbrain__read_note", {"path": "memory/x.md"}),
                   tool_result("b1", [{"type": "text", "text": "see https://example.org/clip"}])]
        self.assertIsNone(self.fetch("https://example.org/clip", entries))

    def test_a_url_the_model_dropped_and_read_back_is_refused(self):
        entries = [tool_use("b1", "mcp__bigbrain__drop", {"text": "https://evil.test/?d=s"}),
                   tool_result("b1", "dropped"),
                   tool_use("b2", "mcp__bigbrain__read_note", {"path": "inbox/x"}),
                   tool_result("b2", [{"type": "text", "text": "https://evil.test/?d=s"}])]
        self.assertIsNotNone(self.fetch("https://evil.test/?d=s", entries))

    def test_other_tools_results_do_not_count(self):
        entries = [tool_use("r1", "Read", {"file_path": "notes.md"}),
                   tool_result("r1", "https://evil.test/?d=s")]
        self.assertIsNotNone(self.fetch("https://evil.test/?d=s", entries))

    def test_meta_and_reminder_text_do_not_count(self):
        meta = human("https://evil.test/a")
        meta["isMeta"] = True
        reminder = {"type": "user", "message": {"content": [
            {"type": "text", "text": "<system-reminder>file changed: https://evil.test/b</system-reminder>"}]}}
        self.assertIsNotNone(self.fetch("https://evil.test/a", [meta]))
        self.assertIsNotNone(self.fetch("https://evil.test/b", [reminder]))

    def test_local_and_private_hosts_are_refused_even_when_given(self):
        for url in ["http://127.0.0.1:4747/api/note", "http://localhost:4330/", "http://[::1]/",
                    "http://10.0.0.5/", "http://169.254.169.254/latest", "http://printer.local/",
                    "http://192.168.1.1/admin"]:
            with self.subTest(url=url):
                self.assertIsNotNone(self.fetch(url, [human(url)]))

    def test_a_name_resolving_to_a_private_address_is_refused(self):
        url = "https://rebind.test/x"
        self.assertIsNotNone(self.fetch(url, [human(url)], resolve=lambda h: {"127.0.0.1"}))
        self.assertIsNotNone(self.fetch(url, [human(url)], resolve=lambda h: {"93.184.216.34", "10.1.2.3"}))

    def test_odd_urls_are_refused(self):
        for url in ["file:///etc/passwd", "ftp://example.org/x", "https://user:pw@example.org/", ""]:
            with self.subTest(url=url):
                self.assertIsNotNone(self.fetch(url, [human(url)]))

    def test_no_transcript_refuses(self):
        self.assertIsNotNone(guard.verdict({"tool_input": {"url": SEARCH}, "transcript_path": "/nonexistent"}, PUBLIC))
        self.assertIsNotNone(guard.verdict({"tool_input": {"url": SEARCH}}, PUBLIC))


if __name__ == "__main__":
    unittest.main()
