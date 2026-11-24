import unittest

from check_pr_shape import problems

TOOL = "M\tcrates/copperpour-core/src/tools/pcb_sync.rs"
NOTE = "A\tdocs/api-migrations/sync-excluded-footprints.md"


class IssueLink(unittest.TestCase):
    def test_closes_on_its_own_line_passes(self):
        self.assertEqual(problems("Summary\n\nCloses #870.\n", []), [])

    def test_part_of_and_cross_repo_pass(self):
        self.assertEqual(problems("Part of #58\n", []), [])
        self.assertEqual(problems("Fixes mixelpixx/Konnect#12\n", []), [])

    def test_forms_github_links_pass(self):
        for body in (
            "- Closes #12\n",
            "**Closes #12**\n",
            "> Fixes #12\n",
            "Closes: #12\n",
            "Resolves https://github.com/Copperpour/Copperpour/issues/12\n",
        ):
            with self.subTest(body=body):
                self.assertEqual(problems(body, []), [])

    def test_link_inside_html_comment_fails(self):
        self.assertEqual(len(problems("<!--\nCloses #12\n-->\n", [])), 1)
        self.assertEqual(len(problems("<!-- unclosed\nCloses #12\n", [])), 1)

    def test_number_on_a_later_line_fails(self):
        self.assertEqual(len(problems("Closes\n#12\n", [])), 1)
        self.assertEqual(len(problems("No issue:\n\nThis PR does X\n", [])), 1)
        self.assertEqual(len(problems("No issue:\n\n## Approach\n", [])), 1)

    def test_example_in_code_block_fails(self):
        for body in (
            "Example:\n\n```markdown\nCloses #12\n```\n",
            "Example:\n\n~~~\nCloses #12\n~~~\n",
            "Example:\n\n```\nCloses #12\n",
            "Example:\n\n    Closes #12\n",
        ):
            with self.subTest(body=body):
                self.assertEqual(len(problems(body, [])), 1)

    def test_declaration_after_code_block_passes(self):
        body = "Example:\n\n```\nNo issue: example\n```\n\nCloses #12\n"
        self.assertEqual(problems(body, []), [])

    def test_unfilled_template_line_fails(self):
        self.assertEqual(len(problems("Closes # (or Part of #)\n", [])), 1)

    def test_no_issue_with_reason_passes(self):
        self.assertEqual(problems("No issue: dependency bump\n", []), [])

    def test_prose_issue_line_fails(self):
        self.assertEqual(len(problems("Issue: #870\n", [])), 1)

    def test_closing_word_inside_prose_fails(self):
        self.assertEqual(len(problems("This also closes #870 later.\n", [])), 1)

    def test_empty_no_issue_fails(self):
        self.assertEqual(len(problems("No issue:\n", [])), 1)


class MigrationNote(unittest.TestCase):
    BODY = "Closes #1\n"

    def test_tool_change_without_note_fails(self):
        found = problems(self.BODY, [TOOL])
        self.assertEqual(len(found), 1)
        self.assertIn("pcb_sync.rs", found[0])

    def test_tool_change_with_new_or_edited_note_passes(self):
        self.assertEqual(problems(self.BODY, [TOOL, NOTE]), [])
        edited = "M\tdocs/api-migrations/sync-excluded-footprints.md"
        self.assertEqual(problems(self.BODY, [TOOL, edited]), [])

    def test_readme_or_deleted_note_does_not_count(self):
        readme = "M\tdocs/api-migrations/README.md"
        deleted = "D\tdocs/api-migrations/old.md"
        self.assertEqual(len(problems(self.BODY, [TOOL, readme, deleted])), 1)

    def test_stated_none_passes(self):
        body = self.BODY + "API migration: none — internal refactor\n"
        self.assertEqual(problems(body, [TOOL]), [])

    def test_none_inside_html_comment_fails(self):
        body = self.BODY + "<!--\nAPI migration: none — internal\n-->\n"
        self.assertEqual(len(problems(body, [TOOL])), 1)

    def test_reason_on_a_later_line_fails(self):
        for body in (
            self.BODY + "API migration: none\n\n## Validation\n",
            self.BODY + "API migration: none —\n\n## Validation\n",
        ):
            with self.subTest(body=body):
                self.assertEqual(len(problems(body, [TOOL])), 1)

    def test_none_in_code_block_fails(self):
        body = self.BODY + "```\nAPI migration: none — internal refactor\n```\n"
        self.assertEqual(len(problems(body, [TOOL])), 1)

    def test_bare_none_fails(self):
        self.assertEqual(len(problems(self.BODY + "API migration: none\n", [TOOL])), 1)

    def test_template_placeholders_fail(self):
        body = "No issue: <reason>\nAPI migration: none — <reason>\n"
        self.assertEqual(len(problems(body, [TOOL])), 2)
        body = "No issue: <reason>\nAPI migration: none: <reason>\n"
        self.assertEqual(len(problems(body, [TOOL])), 2)

    def test_test_only_and_other_crates_need_no_note(self):
        files = [
            "M\tcrates/copperpour-core/src/tools/sch_components/native_dnp_tests.rs",
            "M\tcrates/copperpour-core/src/tools/tests.rs",
            "A\tcrates/copperpour-core/tests/fixtures/x.kicad_sch",
            "M\tcrates/copperpour-sexp/src/writer.rs",
            "M\tREADME.md",
        ]
        self.assertEqual(problems(self.BODY, files), [])


if __name__ == "__main__":
    unittest.main()
