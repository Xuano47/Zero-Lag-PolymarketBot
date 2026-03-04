import json
from eth_account import Account
from eth_account.messages import encode_typed_data

# The fixed test inputs
PRIVATE_KEY = "0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
MAKER_ADDRESS = Account.from_key(PRIVATE_KEY).address

domain_data = {
    "name": "Polymarket CTF Exchange",
    "version": "1",
    "chainId": 137,
    "verifyingContract": "0x4bFB41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E"
}

order_type = [
    {"name": "salt", "type": "uint256"},
    {"name": "maker", "type": "address"},
    {"name": "signer", "type": "address"},
    {"name": "taker", "type": "address"},
    {"name": "tokenId", "type": "uint256"},
    {"name": "makerAmount", "type": "uint256"},
    {"name": "takerAmount", "type": "uint256"},
    {"name": "expiration", "type": "uint256"},
    {"name": "nonce", "type": "uint256"},
    {"name": "feeRateBps", "type": "uint256"},
    {"name": "side", "type": "uint8"},
    {"name": "signatureType", "type": "uint8"}
]

# Exact same values used in the rust test
message = {
    "salt": 1700000000,
    "maker": MAKER_ADDRESS,
    "signer": MAKER_ADDRESS,
    "taker": "0x0000000000000000000000000000000000000000",
    "tokenId": 12345678901234567890,
    "makerAmount": 1000000,
    "takerAmount": 2000000,
    "expiration": 0,
    "nonce": 0,
    "feeRateBps": 0,
    "side": 0, # BUY
    "signatureType": 0 # EOA
}

structured_data = {
    "types": {
        "EIP712Domain": [
            {"name": "name", "type": "string"},
            {"name": "version", "type": "string"},
            {"name": "chainId", "type": "uint256"},
            {"name": "verifyingContract", "type": "address"}
        ],
        "Order": order_type
    },
    "primaryType": "Order",
    "domain": domain_data,
    "message": message
}

signable_message = encode_typed_data(full_message=structured_data)
signed_message = Account.sign_message(signable_message, private_key=PRIVATE_KEY)

# Adjust v to be 27 or 28 for legacy Ethereum compat (EIP-712 requires this for Polygon)
v = signed_message.v
r = hex(signed_message.r)[2:].zfill(64)
s = hex(signed_message.s)[2:].zfill(64)
v_hex = hex(v)[2:].zfill(2)

signature = f"0x{r}{s}{v_hex}"

print(json.dumps({
    "maker": MAKER_ADDRESS,
    "signature": signature,
    "hash": "0x" + signable_message.body.hex()
}, indent=2))
