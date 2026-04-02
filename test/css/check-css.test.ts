/**
 * CSS coverage check.
 *
 * For every full template (extends base.html), collects the CSS files it loads
 * (always base.css, plus whatever is in its {% block styles %} <link> tags),
 * then verifies that every *static* class name used in the template's HTML is
 * actually defined in one of those files.
 *
 * Partial templates (templates/partials/*.html) are rendered as standalone HTML
 * fragments by Rust — they have no {% extends %} and no {% block styles %}.
 * They declare their CSS dependency explicitly on the first line:
 *   <!-- css: breaker.css -->
 * Their classes are checked against base.css + those declared files.
 * A missing declaration is itself a test failure.
 *
 * Dynamic classes — anything inside {{ }} or {% %} expressions — are stripped
 * before analysis and therefore silently skipped. No manual allow-list is needed:
 * if a class is hardcoded in a `class="…"` attribute it must exist in CSS;
 * if it comes from a Rust/template expression it's the author's responsibility.
 *
 * Adding a new template or CSS file requires no changes here — everything is
 * auto-discovered.
 */

import { assertEquals } from "jsr:@std/assert";
import { walk } from "jsr:@std/fs";
import { join } from "jsr:@std/path";

const TEMPLATES_DIR = "templates";
const CSS_DIR = "assets/css";
const BASE_CSS = "base.css";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** All CSS class selectors (.name) defined in a stylesheet. */
async function definedClasses(cssPath: string): Promise<Set<string>> {
    const text = await Deno.readTextFile(cssPath);
    const out = new Set<string>();
    // Matches `.foo` wherever it appears as a selector token.
    // The negative lookbehind avoids matching inside strings/content values.
    for (const m of text.matchAll(/(?<!['"(])\.(-?[a-zA-Z_][\w-]*)/g)) {
        out.add(m[1]);
    }
    return out;
}

/** CSS filenames referenced in a template's {% block styles %} link tags. */
function linkedCssFiles(templateText: string): string[] {
    const files: string[] = [];
    for (const m of templateText.matchAll(/\/assets\/css\/([\w-]+\.css)/g)) {
        files.push(m[1]);
    }
    return files;
}

/**
 * Static class names used in `class="…"` attributes of a template.
 * Template expressions ({{ … }} and {% … %}) are stripped first so only
 * literal tokens remain.
 */
function usedClasses(templateText: string): Set<string> {
    const out = new Set<string>();
    for (const m of templateText.matchAll(/class="([^"]*)"/g)) {
        const literal = m[1]
            .replace(/\{\{[^}]*\}\}/g, "")
            .replace(/\{%-?[^%]*-?%\}/g, "");
        for (const cls of literal.split(/\s+/).filter(Boolean)) {
            // Skip prefix fragments left over from {{ expr }} stripping, e.g.
            // class="ts-state-{{ val }}" → "ts-state-" after strip (not a real class).
            if (cls.endsWith("-")) continue;
            out.add(cls);
        }
    }
    return out;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

Deno.test("CSS coverage: every static class in a full template is defined in its loaded CSS", async () => {
    const baseDefined = await definedClasses(join(CSS_DIR, BASE_CSS));
    const failures: string[] = [];

    for await (const entry of walk(TEMPLATES_DIR, {
        exts: [".html"],
        skip: [/[/\\]partials[/\\]/],
    })) {
        const text = await Deno.readTextFile(entry.path);

        // Build the union of classes available to this template.
        const defined = new Set(baseDefined);
        for (const file of linkedCssFiles(text)) {
            const cssPath = join(CSS_DIR, file);
            try {
                for (const cls of await definedClasses(cssPath)) {
                    defined.add(cls);
                }
            } catch {
                failures.push(`${entry.name}: links to "${file}" but that file does not exist`);
            }
        }

        for (const cls of usedClasses(text)) {
            if (!defined.has(cls)) {
                failures.push(`${entry.name}: class "${cls}" used but not defined in loaded CSS`);
            }
        }
    }

    assertEquals(
        failures,
        [],
        "CSS coverage failures:\n" + failures.map((f) => `  • ${f}`).join("\n"),
    );
});

Deno.test("CSS coverage: every static class in a partial is defined in its declared CSS", async () => {
    // Partials declare their CSS dependency with a comment on the first line:
    //   <!-- css: breaker.css -->
    // The checker loads base.css + those files and checks all static classes.
    // Missing the declaration is itself a failure — it forces the author to be explicit.
    const baseDefined = await definedClasses(join(CSS_DIR, BASE_CSS));
    const failures: string[] = [];

    for await (const entry of walk(join(TEMPLATES_DIR, "partials"), { exts: [".html"] })) {
        const text = await Deno.readTextFile(entry.path);

        // Parse <!-- css: foo.css, bar.css --> from the first line.
        const declaration = text.match(/^<!--\s*css:\s*([^-]+)-->/);
        if (!declaration) {
            failures.push(`${entry.name}: missing CSS declaration (add <!-- css: filename.css --> as the first line)`);
            continue;
        }
        const declaredFiles = declaration[1].split(",").map((s) => s.trim()).filter(Boolean);

        const defined = new Set(baseDefined);
        for (const file of declaredFiles) {
            const cssPath = join(CSS_DIR, file);
            try {
                for (const cls of await definedClasses(cssPath)) {
                    defined.add(cls);
                }
            } catch {
                failures.push(`${entry.name}: declares "${file}" but that file does not exist`);
            }
        }

        for (const cls of usedClasses(text)) {
            if (!defined.has(cls)) {
                failures.push(`${entry.name}: class "${cls}" used but not defined in base.css or ${declaredFiles.join(", ")}`);
            }
        }
    }

    assertEquals(
        failures,
        [],
        "CSS coverage failures (partials):\n" + failures.map((f) => `  • ${f}`).join("\n"),
    );
});
