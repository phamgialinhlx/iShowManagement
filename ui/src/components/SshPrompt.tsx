import { useEffect, useRef, useState } from "react";
import { motion, AnimatePresence } from "motion/react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

import { hostFromPrompt, passwordUsed } from "../lib/ssh-key-offer";

/**
 * The credential dialog for SSH.
 *
 * When `ssh` needs a password, a key passphrase or a 2FA code, the askpass
 * helper routes the prompt here. The message is OpenSSH's own text, shown
 * verbatim — it names the host and account, which is exactly what tells the user
 * whether this prompt is expected.
 *
 * Dismissing sends no answer, which makes the helper exit non-zero and `ssh`
 * abort cleanly. Sending an empty string instead would burn an authentication
 * attempt against the server.
 */

type PromptKind = "password" | "confirm" | "challenge";

type Prompt = {
  id: string;
  message: string;
  kind: PromptKind;
};

export function SshPrompt() {
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [value, setValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const unlisten = listen<Prompt>("ssh://prompt", (event) => {
      setValue("");
      setPrompt(event.payload);
    });
    return () => {
      void unlisten.then((off) => off());
    };
  }, []);

  useEffect(() => {
    if (prompt) inputRef.current?.focus();
  }, [prompt]);

  const respond = async (answer: string | null) => {
    const id = prompt?.id;
    // A password was actually supplied — so this host authenticates by password,
    // which is the one thing rmux cannot know any other way. Recorded *before*
    // the state is cleared, and given only the host: the secret itself never
    // comes near the offer.
    if (answer !== null && prompt?.kind === "password") {
      const host = hostFromPrompt(prompt.message);
      if (host) passwordUsed(host, prompt.message);
    }
    // Clear first: the secret should not sit in component state while the IPC
    // round-trip completes.
    setPrompt(null);
    setValue("");
    if (id) await invoke("answer_prompt", { id, answer });
  };

  return (
    <AnimatePresence>
      {prompt && (
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.15 }}
          className="fixed inset-0 z-[100] grid place-items-center"
          style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
        >
          <motion.form
            initial={{ opacity: 0, y: 12 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ type: "spring", stiffness: 260, damping: 26 }}
            onSubmit={(e) => {
              e.preventDefault();
              void respond(value);
            }}
            className="window corner w-full max-w-[440px] p-6 flex flex-col gap-4"
          >
            <header className="flex flex-col gap-1">
              <span className="kicker">
                {prompt.kind === "confirm"
                  ? "Confirm host"
                  : prompt.kind === "challenge"
                    ? "Second factor"
                    : "Authentication"}
              </span>
            </header>

            {/* OpenSSH's own wording, preserved including line breaks — a
                host-key warning loses its meaning if reflowed or paraphrased. */}
            <p className="data text-[12px] leading-relaxed whitespace-pre-wrap">
              {prompt.message}
            </p>

            <input
              ref={inputRef}
              className="field"
              // Never echo a secret. A yes/no host confirmation is not secret and
              // is far easier to answer when visible.
              type={prompt.kind === "confirm" ? "text" : "password"}
              value={value}
              onChange={(e) => setValue(e.target.value)}
              autoComplete="off"
              spellCheck={false}
              onKeyDown={(e) => {
                if (e.key === "Escape") void respond(null);
              }}
            />

            <div className="flex gap-2">
              <button className="btn btn-primary flex-1" type="submit">
                {prompt.kind === "confirm" ? "Confirm" : "Authenticate"}
              </button>
              <button className="btn" type="button" onClick={() => void respond(null)}>
                Cancel
              </button>
            </div>
          </motion.form>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
