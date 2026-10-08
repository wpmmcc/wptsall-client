#!/usr/bin/env python3
"""Drive the real shared review and Desktop browser adapter in an owned session."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from urllib.parse import urlencode, urlparse

ROOT = Path(__file__).resolve().parents[5]
parser = argparse.ArgumentParser()
parser.add_argument("--server", type=Path, required=True)
parser.add_argument("--session", required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
assert args.server.is_absolute() and args.output.is_absolute()
assert not args.output.exists(), "Never replace earlier browser evidence"
metadata = json.loads(args.server.read_text())
base = metadata["base"]
assert urlparse(base).hostname == "127.0.0.1", "Owned loopback fixture only"
records = []
calls = []


def browser(*command, script=None):
    invocation = ["agent-browser", "--session", args.session, "--json", *command]
    result = subprocess.run(
        invocation, input=script, text=True, capture_output=True, cwd=ROOT, timeout=45
    )
    calls.append({"command": list(command), "exit": result.returncode,
                  "stdout": result.stdout, "stderr": result.stderr})
    if result.returncode != 0:
        raise AssertionError(f"Owned browser command failed: {command}: {result.stdout}{result.stderr}")
    payload = json.loads(result.stdout)
    if not payload["success"]:
        raise AssertionError(payload)
    return payload.get("data", {})


def evaluate(script):
    return browser("eval", "--stdin", script=script)["result"]


def state():
    return evaluate("""
      ({
        title:document.title,locale:document.documentElement.lang,
        entries:Array.from(document.querySelectorAll('[data-testid^="review-entry-"]'))
          .map(el=>({id:el.dataset.testid,value:el.value,disabled:el.disabled})),
        saveDisabled:document.querySelector('[data-testid="review-save"]')?.disabled,
        approve:!!document.querySelector('[data-testid="review-approve"]'),
        retranslate:!!document.querySelector('[data-testid="review-retranslate"]'),
        reject:!!document.querySelector('[data-testid="review-reject"]'),
        delivery:!!document.querySelector('[data-testid="review-delivery-unresolved"]'),
        fault:!!document.querySelector('[data-testid="review-pack-fault"]'),
        confirmCount:window.ownedConfirmCount,errors:window.ownedErrors,
        text:document.getElementById('review').innerText,
        overflow:document.documentElement.scrollWidth>innerWidth
      })
    """)


def evidence(key):
    return evaluate(f"(async()=> (await (await fetch('/__owned_evidence')).json())[{json.dumps(key)}])()")


try:
    for client in ("webui", "desktop"):
        for lang in ("en", "zh-CN"):
            for mode in ("edit", "bad-save", "manual", "new", "delivery", "lost", "invalid"):
                key = f"{args.output.stem}-{client}-{lang}-{mode}"
                browser("open", f"{base}/?{urlencode(dict(client=client,lang=lang,mode=mode,case=key))}")
                if mode == "invalid":
                    browser("wait", '[data-testid="review-pack-fault"]')
                else:
                    browser("wait", '[data-testid="review-entry-101"]')
                evaluate("window.ownedConfirmCount=0; window.confirm=()=>{window.ownedConfirmCount++;return true;};")
                before = state()
                assert before["title"] == "Owned pack review" and before["locale"] == lang
                assert before["errors"] == [] and not before["overflow"], before
                if mode != "invalid":
                    assert [entry["value"] for entry in before["entries"]] == ["Button %s", "Menu %s"]
                    for text in ("Owned button context", "Owned menu context", "Hello %s items", "plural_index=1"):
                        assert text in before["text"], text
                    assert "review.pack_hint" not in before["text"]
                browser("snapshot", "-i")
                if mode == "edit":
                    browser("set", "viewport", "1440", "1000")
                    assert not state()["overflow"]
                    browser("set", "viewport", "800", "560")
                    narrow = state()
                    assert not narrow["overflow"]
                    image = args.output.with_name(f"{args.output.stem}-{client}-{lang}.png")
                    assert not image.exists()
                    browser("scrollintoview", '[data-testid="review-entry-101"]')
                    browser("screenshot", str(image), "--full")
                    browser("fill", '[data-testid="review-entry-101"]', "Reviewed button %s")
                    browser("snapshot", "-i")
                    browser("click", '[data-testid="review-approve"]')
                    browser("wait", "--fn", "document.querySelector('[data-testid=\"review-approve\"]') === null")
                    saved = evidence(key)
                    actions = [(call["method"], call["pathname"]) for call in saved["calls"]]
                    save = actions.index(("PUT", "/api/items/8/translated"))
                    approve = actions.index(("POST", "/api/items/8/approve"))
                    assert save < approve
                    assert saved["writebacks"][-1]["entries"] == [
                        {"entry_id": 101, "msgstr": "Reviewed button %s"},
                        {"entry_id": 102, "msgstr": "Menu %s"},
                    ]
                    assert saved["feeSubmits"] == 0
                elif mode == "bad-save":
                    browser("fill", '[data-testid="review-entry-101"]', "Missing token, retain this draft")
                    browser("snapshot", "-i")
                    browser("click", '[data-testid="review-approve"]')
                    browser("wait", "--fn", "document.querySelector('[data-testid=\"review-approve\"]')?.disabled === false")
                    saved = evidence(key)
                    assert saved["writebacks"] == [] and saved["feeSubmits"] == 0
                    assert any(call["method"] == "PUT" and call["pathname"].endswith("/translated") for call in saved["calls"])
                    assert not any(call["pathname"].endswith("/approve") for call in saved["calls"])
                    assert state()["entries"][0]["value"] == "Missing token, retain this draft"
                elif mode in ("manual", "new"):
                    if mode == "manual":
                        assert before["saveDisabled"] and all(entry["disabled"] for entry in before["entries"])
                        assert not before["approve"] and not before["reject"]
                    browser("click", '[data-testid="review-retranslate"]')
                    browser("wait", "--fn", "document.querySelector('[data-testid=\"review-entry-101\"]')?.value === 'Resumed button %s'")
                    saved = evidence(key)
                    submits = [call for call in saved["calls"] if call["pathname"].endswith("/retranslate")]
                    assert len(submits) == 1
                    if mode == "manual":
                        assert submits[0]["body"] == {
                            "request_id": "631b8b74-913a-4972-8892-33a892ce07bb", "resume_only": True,
                        }
                        assert saved["feeSubmits"] == 0 and state()["confirmCount"] == 0
                        assert not any(call["pathname"].endswith("/override") for call in saved["calls"])
                    else:
                        assert saved["feeSubmits"] == 1 and state()["confirmCount"] == 1
                        assert len(submits[0]["body"]["request_id"]) == 36
                elif mode == "delivery":
                    assert before["delivery"] and before["saveDisabled"] and not before["retranslate"] and not before["reject"]
                    assert all(entry["disabled"] for entry in before["entries"])
                    browser("click", '[data-testid="review-approve"]')
                    browser("wait", "--fn", "document.querySelector('[data-testid=\"review-approve\"]') === null")
                    saved = evidence(key)
                    assert len(saved["writebacks"]) == 1 and saved["feeSubmits"] == 0
                    assert not any(call["method"] == "PUT" for call in saved["calls"])
                elif mode == "lost":
                    browser("click", '[data-testid="review-approve"]')
                    browser("wait", '[data-testid="review-delivery-unresolved"]')
                    parked = state()
                    assert parked["saveDisabled"] and not parked["retranslate"] and not parked["reject"]
                    assert all(entry["disabled"] for entry in parked["entries"])
                    browser("snapshot", "-i")
                    browser("find", "role", "button", "click", "--name", "关闭" if lang == "zh-CN" else "Close")
                    browser("snapshot", "-i")
                    browser("click", '[data-testid="review-approve"]')
                    browser("wait", "--fn", "document.querySelector('[data-testid=\"review-approve\"]') === null")
                    saved = evidence(key)
                    assert saved["writebacks"][0] == saved["writebacks"][1]
                    assert saved["feeSubmits"] == 0
                else:
                    assert before["fault"] and before["entries"] == [] and not before["approve"] and not before["retranslate"]
                    saved = evidence(key)
                    assert all(call["method"] == "GET" for call in saved["calls"])
                after = state()
                assert after["errors"] == [] and not after["overflow"], after
                if client == "desktop":
                    assert any(call["command"] == "get_item_content" for call in saved["invokes"])
                    if mode in ("manual", "new"):
                        invoke = next(call for call in saved["invokes"] if call["command"] == "retranslate_item")
                        assert invoke["args"]["request"] == submits[0]["body"]
                records.append({"case":key,"passed":True,"before":before,"after":after,"evidence":saved})
                print(f"PASS {key}", flush=True)
finally:
    fingerprints = {}
    for relative in (
        "client-wpplugin/source/frontend/src/pages/TranslationReview.svelte",
        "client-wpplugin/source/frontend/src/lib/review/languagePack.ts",
        "client-desktop/frontend/src/lib/api/client.ts",
        "tests/modules/client-wpplugin/webui/pack-review/serve.mjs",
        "tests/modules/client-wpplugin/webui/pack-review/check.py",
    ):
        fingerprints[relative] = hashlib.sha256((ROOT / relative).read_bytes()).hexdigest()
    args.output.write_text(json.dumps({
        "cases":records,"calls":calls,"fingerprints":fingerprints,"server":metadata,
        "limitations":"Owned browser mocks and Desktop JS adapter, not native Tauri GUI or real paid provider acceptance.",
    }, ensure_ascii=False, indent=2))
assert len(records) == 28
