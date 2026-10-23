CREATE TABLE IF NOT EXISTS block (
    height INTEGER PRIMARY KEY NOT NULL,
    hash TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tx (
    txid TEXT NOT NULL PRIMARY KEY,
    tx BLOB,
    first_seen INTEGER,
    last_seen INTEGER,
    last_evicted INTEGER
);

CREATE TABLE IF NOT EXISTS txout (
    txid TEXT NOT NULL,
    vout INTEGER NOT NULL,
    value INTEGER NOT NULL,
    script BLOB NOT NULL,
    PRIMARY KEY (txid, vout)
);

CREATE TABLE IF NOT EXISTS anchor (
    block_height INTEGER NOT NULL,
    block_hash TEXT NOT NULL,
    txid TEXT NOT NULL,
    confirmation_time INTEGER NOT NULL,
    PRIMARY KEY (block_height, block_hash, txid)
);

CREATE TABLE IF NOT EXISTS keychain_last_revealed (
    descriptor_id TEXT NOT NULL PRIMARY KEY,
    last_revealed INTEGER
);

CREATE TABLE IF NOT EXISTS keychain_script_pubkey (
    descriptor_id TEXT NOT NULL,
    derivation_index INTEGER,
    script BLOB,
    PRIMARY KEY (descriptor_id, derivation_index)
);

CREATE TABLE IF NOT EXISTS keychain (
    keychain INTEGER NOT NULL PRIMARY KEY,
    descriptor TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS network (
    id INTEGER NOT NULL DEFAULT 0 CHECK (id = 0),
    network TEXT NOT NULL,
    PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS locked_outpoint (
    txid TEXT NOT NULL,
    vout INTEGER NOT NULL,
    PRIMARY KEY (txid, vout)
);
