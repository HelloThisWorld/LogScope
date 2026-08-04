// v0.3 disclosure profiles: typed rule rows serialized to the stored
// documents, compile-validated Rust-side at every save. Attaching a
// profile to a report definition and previewing the exact final bytes
// happen in Reports.tsx; this panel manages the profiles themselves.

import { useCallback, useEffect, useState } from "react";
import { api, errorText } from "./api";
import type { RedactionProfileDto } from "./api";

export const RULE_KINDS = [
  "omit_field",
  "mask_field",
  "replace_exact",
  "replace_regex",
  "pseudonymize",
] as const;

type RuleRow = {
  kind: (typeof RULE_KINDS)[number];
  field: string;
  find: string;
  replace: string;
  pattern: string;
  prefix: string;
};

function emptyRule(): RuleRow {
  return {
    kind: "mask_field",
    field: "",
    find: "",
    replace: "",
    pattern: "",
    prefix: "subject",
  };
}

function rulesToJson(rules: RuleRow[]): string {
  return JSON.stringify(
    rules.map((r) => {
      switch (r.kind) {
        case "omit_field":
          return { kind: r.kind, field: r.field };
        case "mask_field":
          return { kind: r.kind, field: r.field };
        case "replace_exact":
          return { kind: r.kind, find: r.find, replace: r.replace };
        case "replace_regex":
          return { kind: r.kind, pattern: r.pattern, replace: r.replace };
        case "pseudonymize":
          return { kind: r.kind, field: r.field, prefix: r.prefix };
      }
    }),
  );
}

function rulesFromJson(json: string): RuleRow[] {
  try {
    const parsed = JSON.parse(json) as Array<Record<string, string>>;
    return parsed.map((p) => ({
      ...emptyRule(),
      kind: (p.kind as RuleRow["kind"]) ?? "mask_field",
      field: p.field ?? "",
      find: p.find ?? "",
      replace: p.replace ?? "",
      pattern: p.pattern ?? "",
      prefix: p.prefix ?? "subject",
    }));
  } catch {
    return [];
  }
}

type PostureDraft = {
  path_policy: "omit" | "basename" | "include";
  field_deny: string;
  field_allow: string;
};

function postureToJson(p: PostureDraft): string {
  const split = (s: string) =>
    s
      .split(",")
      .map((x) => x.trim())
      .filter(Boolean);
  return JSON.stringify({
    path_policy: p.path_policy,
    field_deny: split(p.field_deny),
    field_allow: split(p.field_allow),
  });
}

function postureFromJson(json: string): PostureDraft {
  try {
    const p = JSON.parse(json) as {
      path_policy?: string;
      field_deny?: string[];
      field_allow?: string[];
    };
    return {
      path_policy: (p.path_policy as PostureDraft["path_policy"]) ?? "omit",
      field_deny: (p.field_deny ?? []).join(", "),
      field_allow: (p.field_allow ?? []).join(", "),
    };
  } catch {
    return { path_policy: "omit", field_deny: "", field_allow: "" };
  }
}

export default function RedactionPanel({
  onChanged,
}: {
  onChanged: () => void;
}) {
  const [profiles, setProfiles] = useState<RedactionProfileDto[]>([]);
  const [status, setStatus] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [rules, setRules] = useState<RuleRow[]>([]);
  const [posture, setPosture] = useState<PostureDraft>({
    path_policy: "omit",
    field_deny: "",
    field_allow: "",
  });

  const load = useCallback(async () => {
    try {
      setProfiles(await api.listRedactionProfiles());
    } catch (e) {
      setStatus(errorText(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const openEditor = (p: RedactionProfileDto | null) => {
    if (p) {
      setEditingId(p.profile_id);
      setName(p.name);
      setRules(rulesFromJson(p.rules_json));
      setPosture(postureFromJson(p.posture_json));
    } else {
      setEditingId("");
      setName("");
      setRules([emptyRule()]);
      setPosture({ path_policy: "omit", field_deny: "", field_allow: "" });
    }
  };

  const save = async () => {
    setStatus("");
    try {
      if (editingId === "") {
        await api.createRedactionProfile(
          name.trim(),
          rulesToJson(rules),
          postureToJson(posture),
        );
      } else if (editingId) {
        const existing = profiles.find((p) => p.profile_id === editingId);
        if (!existing) return;
        await api.updateRedactionProfile(
          editingId,
          existing.revision,
          name.trim(),
          rulesToJson(rules),
          postureToJson(posture),
        );
      }
      setEditingId(null);
      await load();
      onChanged();
      setStatus("profile saved (validated by compiling the projection)");
    } catch (e) {
      setStatus(errorText(e));
    }
  };

  const setRule = (i: number, patch: Partial<RuleRow>) =>
    setRules((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)));

  return (
    <>
      <h4>Disclosure profiles</h4>
      <p className="dim">
        A profile is an export-time projection: it shapes what leaves the
        workspace and never mutates canonical data. Any rule or posture
        change bumps the profile version, and every generated artifact
        names the version that shaped it.
      </p>
      {status && (
        <div className="dim" role="status">
          {status}
        </div>
      )}
      <div className="row">
        <button onClick={() => openEditor(null)}>New profile</button>
      </div>
      <ul className="case-cards">
        {profiles.map((p) => (
          <li key={p.profile_id} className="case-card">
            <div className="row">
              <strong>{p.name}</strong>
              <span className="dim">
                v{p.profile_version} · {rulesFromJson(p.rules_json).length}{" "}
                rules
              </span>
              <span className="spacer" />
              <button onClick={() => openEditor(p)}>edit</button>
            </div>
          </li>
        ))}
        {profiles.length === 0 && (
          <li className="dim">no disclosure profiles yet</li>
        )}
      </ul>

      {editingId !== null && (
        <div className="case-form">
          <label>
            Name
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              aria-label="Profile name"
            />
          </label>

          <strong className="dim">Rules (applied in order)</strong>
          {rules.map((r, i) => (
            <div key={i} className="row">
              <select
                value={r.kind}
                aria-label="Rule kind"
                onChange={(e) =>
                  setRule(i, { kind: e.target.value as RuleRow["kind"] })
                }
              >
                {RULE_KINDS.map((k) => (
                  <option key={k} value={k}>
                    {k}
                  </option>
                ))}
              </select>
              {(r.kind === "omit_field" ||
                r.kind === "mask_field" ||
                r.kind === "pseudonymize") && (
                <input
                  value={r.field}
                  placeholder="field name"
                  aria-label="Field name"
                  onChange={(e) => setRule(i, { field: e.target.value })}
                />
              )}
              {r.kind === "pseudonymize" && (
                <input
                  value={r.prefix}
                  placeholder="prefix"
                  aria-label="Pseudonym prefix"
                  size={10}
                  onChange={(e) => setRule(i, { prefix: e.target.value })}
                />
              )}
              {r.kind === "replace_exact" && (
                <>
                  <input
                    value={r.find}
                    placeholder="find (exact)"
                    aria-label="Find text"
                    onChange={(e) => setRule(i, { find: e.target.value })}
                  />
                  <input
                    value={r.replace}
                    placeholder="replace with"
                    aria-label="Replacement"
                    onChange={(e) => setRule(i, { replace: e.target.value })}
                  />
                </>
              )}
              {r.kind === "replace_regex" && (
                <>
                  <input
                    value={r.pattern}
                    placeholder="regex (bounded, linear-time)"
                    aria-label="Regex pattern"
                    onChange={(e) => setRule(i, { pattern: e.target.value })}
                  />
                  <input
                    value={r.replace}
                    placeholder="replace with"
                    aria-label="Replacement"
                    onChange={(e) => setRule(i, { replace: e.target.value })}
                  />
                </>
              )}
              <button
                aria-label="Remove rule"
                onClick={() => setRules((rs) => rs.filter((_, j) => j !== i))}
              >
                ×
              </button>
            </div>
          ))}
          <div className="row">
            <button onClick={() => setRules((rs) => [...rs, emptyRule()])}>
              + rule
            </button>
          </div>

          <strong className="dim">Posture</strong>
          <div className="row">
            <label>
              Provenance paths
              <select
                value={posture.path_policy}
                aria-label="Path policy"
                onChange={(e) =>
                  setPosture({
                    ...posture,
                    path_policy: e.target
                      .value as PostureDraft["path_policy"],
                  })
                }
              >
                <option value="omit">omit (default-closed)</option>
                <option value="basename">basename only</option>
                <option value="include">include full paths</option>
              </select>
            </label>
            <label>
              Deny fields
              <input
                value={posture.field_deny}
                placeholder="comma, separated"
                aria-label="Denied fields"
                onChange={(e) =>
                  setPosture({ ...posture, field_deny: e.target.value })
                }
              />
            </label>
            <label>
              Allow-only fields
              <input
                value={posture.field_allow}
                placeholder="empty = all except denied"
                aria-label="Allowed fields"
                onChange={(e) =>
                  setPosture({ ...posture, field_allow: e.target.value })
                }
              />
            </label>
          </div>

          <div className="row">
            <button onClick={() => void save()} disabled={!name.trim()}>
              Save profile
            </button>
            <button onClick={() => setEditingId(null)}>Cancel</button>
          </div>
        </div>
      )}
    </>
  );
}
