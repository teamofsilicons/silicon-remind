"""Exercise provisioning SQL without AWS credentials or database access."""
import ast
from pathlib import Path
import unittest


class BootstrapGrants(unittest.TestCase):
    def generated_sql(self, testing):
        source = Path(__file__).with_name("bootstrap-task.py").read_text()
        function = next(node for node in ast.parse(source).body
                        if isinstance(node, ast.FunctionDef) and node.name == "configure")
        calls = []
        scope = {"db_env": lambda *args: {},
                 "command": lambda args, env, sql: calls.append(sql),
                 "print": lambda *args, **kwargs: None}
        exec(compile(ast.Module(body=[function], type_ignores=[]), "configure", "exec"), scope)
        scope["configure"]({}, "fixture-host",
                           "silicon_remind_test" if testing else "silicon_remind",
                           "remind_testing" if testing else "remind_runtime",
                           "synthetic", testing=testing)
        self.assertEqual(len(calls), 1)
        return calls[0]

    def test_testing_migrator_can_create_transaction_local_mapping_tables(self):
        sql = self.generated_sql(True)
        self.assertIn("GRANT CREATE, TEMPORARY ON DATABASE silicon_remind_test TO remind_testing;", sql)
        self.assertIn("REVOKE ALL ON DATABASE silicon_remind_test FROM PUBLIC;", sql)
        self.assertNotIn("TO PUBLIC", sql)
        self.assertIn("NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOINHERIT NOBYPASSRLS", sql)

    def test_production_role_still_receives_only_database_connect(self):
        sql = self.generated_sql(False)
        self.assertIn("GRANT CONNECT ON DATABASE silicon_remind TO remind_runtime;", sql)
        self.assertNotIn("TEMPORARY", sql)
        self.assertNotIn("GRANT CREATE", sql)
        self.assertNotIn("TO PUBLIC", sql)


if __name__ == "__main__":
    unittest.main()
