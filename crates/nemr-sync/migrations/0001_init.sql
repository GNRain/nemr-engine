-- WP-J initial schema: identity, session index, lease.
--
-- Postgres from day one (D-03): the lease needs real concurrency semantics —
-- atomic takeover, TTL expiry, two clients racing — that SQLite cannot give.
-- The lease statements below rely on row-level locking and single-statement
-- upserts that are atomic under concurrent transactions.

-- --- Identity -------------------------------------------------------------
--
-- The server stores ciphertext it cannot read (E-16). What it holds per user:
-- a public KDF salt + params, an Argon2id verifier over the client-derived
-- auth key, and two opaque envelopes (password and recovery) wrapping a master
-- key the server never sees. `recovery_ack_hash` is SHA-256(domain || MK); the
-- account is not usable until the client proves, via the recovery envelope,
-- that recovery recovers the same MK.
CREATE TABLE users (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email              TEXT NOT NULL UNIQUE,            -- stored lower-cased
    kdf_salt           BYTEA NOT NULL,
    kdf_m_cost         INTEGER NOT NULL,
    kdf_t_cost         INTEGER NOT NULL,
    kdf_p_cost         INTEGER NOT NULL,
    auth_verifier      TEXT NOT NULL,                   -- Argon2id PHC string
    password_envelope  BYTEA NOT NULL,
    recovery_salt      BYTEA NOT NULL,
    recovery_m_cost    INTEGER NOT NULL,
    recovery_t_cost    INTEGER NOT NULL,
    recovery_p_cost    INTEGER NOT NULL,
    recovery_envelope  BYTEA NOT NULL,
    recovery_ack_hash  BYTEA NOT NULL,
    status             TEXT NOT NULL DEFAULT 'pending_recovery',  -- | 'active'
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Bearer tokens. Opaque random bytes; only their SHA-256 is stored, so a
-- database leak does not yield a usable token. Expiry is enforced on lookup.
CREATE TABLE auth_tokens (
    token_hash  BYTEA PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL
);
CREATE INDEX auth_tokens_user_idx ON auth_tokens(user_id);

-- Login attempts, for rate limiting. One row per attempt; the limiter counts
-- recent failures per email within a window. Pruned opportunistically.
CREATE TABLE login_attempts (
    id           BIGSERIAL PRIMARY KEY,
    email        TEXT NOT NULL,
    succeeded    BOOLEAN NOT NULL,
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX login_attempts_email_time_idx ON login_attempts(email, attempted_at);

-- --- Session index --------------------------------------------------------
--
-- Unencrypted metadata so the list works on any machine (E-16 keeps only this
-- readable). The bundle itself is ciphertext at `storage_key`.
CREATE TABLE sessions (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id            UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    agent              TEXT NOT NULL,
    size_bytes         BIGINT NOT NULL DEFAULT 0,
    description        TEXT NOT NULL DEFAULT '',
    base_image_version TEXT NOT NULL DEFAULT '',
    last_machine       TEXT,                    -- which machine last held it
    storage_key        TEXT,                    -- null until first upload
    ciphertext_sha256  BYTEA,                   -- integrity of the stored blob
    ciphertext_bytes   BIGINT,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, name)
);

-- --- Lease (D-03) ---------------------------------------------------------
--
-- One row per session. `fence` is a monotonic token bumped on every acquire and
-- takeover but NOT on a heartbeat renewal. A holder carries (holder, fence); a
-- write or a heartbeat is honoured only if both still match the row and it has
-- not expired. When another client takes over, `fence` advances, and the loser's
-- next heartbeat and any write it attempts are refused — server-side, not merely
-- by the client's own good behaviour.
CREATE TABLE leases (
    session_id  UUID PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    holder      TEXT NOT NULL,
    fence       BIGINT NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL
);
