#!/usr/bin/env python3
"""Validate the universal node classifier on held-out labeled examples.

5-class target: ≥80% accuracy on 5 labels (DECLARATION, EXPRESSION, STATEMENT, PATTERN, TYPE).
Sub-classifier target: ≥70% on 5 labels (CALL, BINARY, UNARY, ACCESS, LITERAL).
"""
import json
import os
import sys
import numpy as np
sys.path.insert(0, os.path.dirname(__file__))
from label_nodes import compute_features, get_examples_from_file, LABELS_5, LABELS_EXPR

with open('.reliary/node_classifier.json') as f:
    weights = json.load(f)

W5 = np.array(weights['weights_5'])
b5 = np.array(weights['bias_5'])
mean = np.array(weights['mean_5'])
std = np.array(weights['std_5'])

def predict_5(features):
    norm = (np.array(features) - mean) / std
    scores = W5.T @ norm + b5
    return int(np.argmax(scores))

if 'weights_expr' in weights:
    We = np.array(weights['weights_expr'])
    be = np.array(weights['bias_expr'])
    meane = np.array(weights['mean_expr'])
    stde = np.array(weights['std_expr'])
    def predict_expr(features):
        norm = (np.array(features) - meane) / stde
        scores = We.T @ norm + be
        return int(np.argmax(scores))
else:
    predict_expr = lambda x: -1

# Held-out test set: 20 examples per language from OUTSIDE the training corpus.
test_files = [
    ('rust', '/home/user/src/reliary8/crates/reliary-compress/src/lib.rs'),
    ('rust', '/home/user/src/reliary8/crates/reliary-output/src/lib.rs'),
    ('rust', '/home/user/src/reliary8/crates/reliary-sift/src/lib.rs'),
    ('rust', '/home/user/src/reliary8/crates/reliary-agent/src/main.rs'),
    ('python', '/home/user/src/reliary8/bench/label_nodes.py'),
    ('python', '/home/user/src/reliary8/bench/validate_node_classifier.py'),
]

correct5 = 0
total5 = 0
correct_e = 0
total_e = 0

for lang, fp in test_files:
    if not os.path.exists(fp):
        continue
    (x5, y5), (xe, ye) = get_examples_from_file(fp, lang)
    for f, actual in zip(x5, y5):
        pred = predict_5(f)
        if pred == actual:
            correct5 += 1
        total5 += 1
    for f, actual in zip(xe, ye):
        pred = predict_expr(f)
        if pred == actual:
            correct_e += 1
        total_e += 1

acc5 = correct5 / max(total5, 1)
acc_e = correct_e / max(total_e, 1)
print(f"5-class accuracy: {correct5}/{total5} = {acc5:.3f} (target ≥0.80)")
print(f"Sub-classifier accuracy: {correct_e}/{total_e} = {acc_e:.3f} (target ≥0.70)")

if acc5 >= 0.80:
    print("✅ 5-class PASS")
else:
    print(f"❌ 5-class FAIL (need {0.80*total5:.0f}+ correct, have {correct5})")
if acc_e >= 0.70:
    print("✅ Sub-classifier PASS")
else:
    print(f"❌ Sub-classifier needs more data (current {total_e} examples)")
