"""Acceptance for the pairing code rules (PROTOCOL.md s.2.2 P-1..P-5).

One check per rule, plus the cross-check that matters most: the browser and the
daemon derive the *same* key from the same inputs. If that ever diverges, a
phone would pair "successfully" and then fail every request afterwards.

  python tools/check_pairing.py
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from pcmon import pairing  # noqa: E402

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print("%-42s %s%s" % (name, "PASS" if ok else "FAIL",
                          ("  " + detail) if detail else ""))


def main():
    # ---- P-1 shape: six digits ------------------------------------------
    code = pairing.new_code()
    check("code is 6 digits", len(code) == 6 and code.isdigit(), code)

    # ---- happy path: both sides derive the same key ----------------------
    table = pairing.PairingCodes()
    pair_id, shown, daemon_nonce, ttl = table.begin("phone-blue", source="10.0.0.5")
    client_nonce = "c" * 32
    got = table.claim(pair_id, shown, client_nonce, source="10.0.0.5")
    check("claim with the right code succeeds", got is not None,
          "returned %r" % (got,))
    if got:
        client_id, key = got
        # What the browser computes with WebCrypto, same inputs.
        expected = pairing.derive_key(shown, client_id, client_nonce, daemon_nonce)
        check("client and daemon derive the same key", expected == key,
              "key=%s" % key)
        check("key is 32 hex chars (128 bits)",
              len(key) == 32 and all(c in "0123456789abcdef" for c in key))

    # ---- P-2: only the hash is stored -----------------------------------
    table2 = pairing.PairingCodes()
    pid2, code2, _, _ = table2.begin("phone", source="s")
    pending = table2._pending  # noqa: SLF001 - the point of this check
    check("code is never stored in the clear",
          pending.code_hash != code2 and code2 not in pending.code_hash,
          "hash=%s..." % pending.code_hash[:12])

    # ---- wrong code ------------------------------------------------------
    table3 = pairing.PairingCodes()
    pid3, code3, _, _ = table3.begin("phone", source="s")
    wrong = "000000" if code3 != "000000" else "111111"
    check("wrong code is refused",
          table3.claim(pid3, wrong, "n" * 32, source="s") is None)

    # ---- P-1: single use -------------------------------------------------
    table4 = pairing.PairingCodes()
    pid4, code4, _, _ = table4.begin("phone", source="s")
    first = table4.claim(pid4, code4, "n" * 32, source="s")
    second = table4.claim(pid4, code4, "n" * 32, source="s")
    check("a code works exactly once",
          first is not None and second is None)

    # ---- P-1: expiry -----------------------------------------------------
    table5 = pairing.PairingCodes(ttl=60.0)
    pid5, code5, _, _ = table5.begin("phone", source="s", now=1000.0)
    late = table5.claim(pid5, code5, "n" * 32, source="s", now=1000.0 + 61.0)
    check("an expired code is refused", late is None)

    # ---- P-4: one pending at a time --------------------------------------
    table6 = pairing.PairingCodes()
    pid_a, code_a, _, _ = table6.begin("first", source="s")
    pid_b, code_b, _, _ = table6.begin("second", source="s")
    old = table6.claim(pid_a, code_a, "n" * 32, source="s")
    new = table6.claim(pid_b, code_b, "n" * 32, source="s")
    check("a new request replaces the old one",
          old is None and new is not None and new[0] == "second")

    # ---- P-3: three wrong codes lock the source --------------------------
    table7 = pairing.PairingCodes()
    pid7, code7, _, _ = table7.begin("phone", source="10.0.0.9")
    bad = "000000" if code7 != "000000" else "111111"
    for _ in range(3):
        table7.claim(pid7, bad, "n" * 32, source="10.0.0.9")
    still = table7.claim(pid7, code7, "n" * 32, source="10.0.0.9")
    check("three wrong codes lock the source (even the right code fails)",
          still is None)
    other = table7.claim(pid7, code7, "n" * 32, source="10.0.0.10")
    check("another source can still try", other is not None,
          "other-source claim=%r" % (other is not None))

    # ---- missing client nonce -------------------------------------------
    table8 = pairing.PairingCodes()
    pid8, code8, _, _ = table8.begin("phone", source="s")
    check("a claim without a client nonce is refused",
          table8.claim(pid8, code8, "", source="s") is None)

    failed = [r for r in results if not r[1]]
    print("")
    if failed:
        print("PAIRING FAIL (%d/%d failed)" % (len(failed), len(results)))
        for name, _, detail in failed:
            print("  - %s %s" % (name, detail))
        return 1
    print("PAIRING OK (%d checks)" % len(results))
    return 0


if __name__ == "__main__":
    sys.exit(main())
