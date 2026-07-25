import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("execute_plan.py")
SPEC = importlib.util.spec_from_file_location("execute_plan", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
execute_plan = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = execute_plan
SPEC.loader.exec_module(execute_plan)


class ExecutePlanTests(unittest.TestCase):
    def test_stage_configuration_matches_current_plans(self) -> None:
        execute_plan.validate_stage_definitions()
        self.assertEqual(execute_plan.STAGES[0].stage_id, "0.2.0/A1")
        self.assertEqual(execute_plan.STAGES[-1].stage_id, "0.5.0/C5")

    def test_stage_selection(self) -> None:
        selected = execute_plan.select_stages("0.5.0/A5", None)
        self.assertEqual(
            [stage.stage_id for stage in selected],
            ["0.5.0/A5", "0.5.0/B5", "0.5.0/C5"],
        )
        only = execute_plan.select_stages(None, "0.2.0/A1")
        self.assertEqual([stage.stage_id for stage in only], ["0.2.0/A1"])

    def test_sensitive_path_detection(self) -> None:
        blocked = {
            ".devcontainer/Dockerfile": "automation control-plane path",
            ".env": "environment file",
            "secrets/private.pem": "private-key or certificate file",
            "scripts/execute_plan.py": "automation control-plane path",
            ".local/run/output.json": "generated, private or repository-internal path",
            "target/debug/lumi": "generated, private or repository-internal path",
        }
        for path, reason in blocked.items():
            self.assertEqual(execute_plan.blocked_path_reason(path), reason)
        self.assertIsNone(execute_plan.blocked_path_reason(".env.example"))
        self.assertIsNone(execute_plan.blocked_path_reason("src/monkey.rs"))

    def test_checkpoint_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "state.json"
            state = {
                "version": execute_plan.STATE_VERSION,
                "current_index": 2,
                "stages": {"0.2.0/A1": {"phase": "gate_failed"}},
            }
            execute_plan.atomic_write_json(path, state)
            self.assertEqual(execute_plan.load_state(path), state)

    def test_prompts_forbid_git_history_changes(self) -> None:
        stage = execute_plan.STAGES[0]
        for prompt in (
            execute_plan.implementation_prompt(stage),
            execute_plan.audit_prompt(stage),
        ):
            self.assertIn("git commit", prompt)
            self.assertIn("не переходи", prompt.lower())
            self.assertIn("make", prompt)

    def test_codex_events_are_rendered_for_humans(self) -> None:
        agent = execute_plan.format_codex_event(
            '{"type":"item.completed","item":{"type":"agent_message",'
            '"text":"Проверяю проект"}}\n'
        )
        command = execute_plan.format_codex_event(
            '{"type":"item.started","item":{"type":"command_execution",'
            '"command":"make c","status":"in_progress"}}\n'
        )
        result = execute_plan.format_codex_event(
            '{"type":"item.completed","item":{"type":"command_execution",'
            '"command":"make c","aggregated_output":"ok\\n","exit_code":0,'
            '"status":"completed"}}\n'
        )
        usage = execute_plan.format_codex_event(
            '{"type":"turn.completed","usage":{"input_tokens":100,'
            '"cached_input_tokens":80,"output_tokens":20}}\n'
        )

        self.assertEqual(agent, "[codex]\nПроверяю проект\n")
        self.assertEqual(command, "[codex:command] $ make c\n")
        self.assertEqual(result, "ok\n[codex:command] completed, exit=0\n")
        self.assertIn("input=100", usage or "")
        self.assertEqual(
            execute_plan.format_codex_event("not json\n"),
            "not json\n",
        )

    def test_codex_stream_is_live_and_raw_log_stays_jsonl(self) -> None:
        event = {
            "type": "item.completed",
            "item": {"type": "agent_message", "text": "Готово"},
        }
        with tempfile.TemporaryDirectory() as temp_dir:
            log_path = Path(temp_dir) / "codex.jsonl"
            output = io.StringIO()
            command = (
                sys.executable,
                "-c",
                f"print({json.dumps(json.dumps(event, ensure_ascii=False))})",
            )
            with contextlib.redirect_stdout(output):
                exit_code = execute_plan.stream_process(
                    command,
                    log_path=log_path,
                    display_line=execute_plan.format_codex_event,
                    log_command=False,
                )

            self.assertEqual(exit_code, 0)
            self.assertIn("[codex]\nГотово", output.getvalue())
            raw_events = [
                json.loads(line)
                for line in log_path.read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(raw_events, [event])


if __name__ == "__main__":
    unittest.main()
