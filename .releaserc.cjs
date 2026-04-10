module.exports = {
  branches: ['main'],
  plugins: [
    '@semantic-release/commit-analyzer',
    '@semantic-release/release-notes-generator',
    [
      '@semantic-release/exec',
      {
        prepareCmd:
          'sed -i \'s/^version = ".*"/version = "${nextRelease.version}"/\' Cargo.toml && cargo generate-lockfile',
      },
    ],
    [
      '@semantic-release/changelog',
      {
        changelogFile: 'changelog.md',
      },
    ],
    [
      '@semantic-release/git',
      {
        assets: ['changelog.md', 'Cargo.toml', 'Cargo.lock'],
        message:
          'chore(release): ${nextRelease.version} [skip ci]\n\n${nextRelease.notes}',
      },
    ],
    [
      '@semantic-release/github',
      {
        failComment: false,
        failTitle: false,
      },
    ],
  ],
};
