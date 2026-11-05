"""Seeds N users with known session tokens directly into a Lightleaf database.

Bypasses Argon2 so thousands of users can be created in a second. Never point it at a real
deployment: the accounts have no usable password but valid sessions.

    python3 seed.py <data>/lightleaf.db 1000 tokens.json
"""
import sqlite3, hashlib, sys, time, json
db, n = sys.argv[1], int(sys.argv[2])
c = sqlite3.connect(db)
now = int(time.time())
tokens = []
for i in range(n):
    c.execute("INSERT INTO users (email, name, password_hash, is_admin, created_at) VALUES (?,?,?,?,?)",
              (f"u{i}@load.test", f"User {i}", "x", 0, now))
    uid = c.execute("SELECT last_insert_rowid()").fetchone()[0]
    tok = hashlib.sha256(f"tok{i}".encode()).hexdigest()
    c.execute("INSERT INTO sessions (token_hash, user_id, expires_at, created_at, last_seen, ip, user_agent) VALUES (?,?,?,?,?,?,?)",
              (hashlib.sha256(tok.encode()).hexdigest(), uid, now + 86400, now, now, "", "load"))
    tokens.append(tok)
c.commit()
json.dump(tokens, open(sys.argv[3], "w"))
