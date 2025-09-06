#!/bin/sh

if [ "$1" != "pre-commit" ]; then
    exit 0
fi

# find all staged rust files
staged_files=$(git diff --cached --name-only --diff-filter=ACM | grep '\.rs$')

if [ -z "$staged_files" ]; then
    exit 0
fi

invalid_comments=""
for file in $staged_files; do
    # extract comments that start with single uppercase letter followed by lowercase
    # this excludes: ///, //!, and comments starting with 2+ uppercase letters (abbreviations)
    bad_lines=$(grep -n '^\s*//' "$file" | \
                grep -v '^\s*///' | \
                grep -v '^\s*//!' | \
                sed 's|^\([^:]*:\)\s*// *|\1|' | \
                grep -E '^[^:]*:[A-Z][a-z]')

    if [ -n "$bad_lines" ]; then
        invalid_comments="$invalid_comments\n$file:$bad_lines"
    fi
done

if [ -n "$invalid_comments" ]; then
    echo "Comments should start with lowercase (unless abbreviations):"
    echo "$invalid_comments"
    exit 1
fi

exit 0

