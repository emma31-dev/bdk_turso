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
