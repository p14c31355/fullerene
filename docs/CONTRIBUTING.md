---

# Contribution Guidelines

Welcome! We welcome your contributions to this repository. 🙌
Please follow the guidelines below when reporting bugs, adding features, or making improvements.

---

## 🔧 Setting up the development environment

### Prerequisites

- Rust (Latest version recommended)
- `cargo` / `rustup` already installed

```sh
# If you need Rust nightly
rustup install nightly
```

### Getting dependencies

```sh
cargo check
```

---

## 🐛 Bug report

Please create an issue following the [Bug Report Template](../.github/ISSUE_TEMPLATE/bug_report.md).
If possible, please attach **reproducible code** and **I2C logs**.

---

## ✨ Feature proposal

Please use the [Feature Request Template](../.github/ISSUE_TEMPLATE/feature_request.md) to submit your proposal as an issue.

* Please note the compatibility and limitations with existing drivers.
* Please clearly indicate support for `no_std`.
* Please provide a rationale for any changes to command specifications or additions of macros.

---

## 🔃 Pull request

1. **Create an issue and then create a branch**
   Please name it according to the branch strategy.
   - `develop/**` (for development integration)  
   - `feature/**` (for new features)  
   - `hotfix/**` (for emergency fixes)

2. **Commit message conventions**
   <type>: <short summary>
   <optional longer description>

   type -> feat, fix, chore

3. **Passing tests and `cargo check`**

4. **Describe the explanation according to the PR template.**

5. Please write in the PR comment to close the related issue:

   ```text
   Closes #42
   ```

---

## 🧪 Testing Policy

* The basic requirement is that `cargo test` passes.
* Compilation passes on the `no_std` target.
* Testing on physical hardware (I2C/SPI) is not subject to CI depending on the environment (visual confirmation is OK).

---

## 📦 Coding conventions

* Compliant with `rustfmt`
* We recommend addressing all clippy warnings
* When both `std` and `no_std` are supported, use `#[cfg(feature = 'std')]`.

---

## 🤝 License

This project is licensed under the MIT License and the Apache 2.0 License.
Contributed code will also be released under the MIT License and the Apache 2.0 License.

---

## 💬 Contact

* Maintainer: [p14c31355](https://github.com/p14c31355)
* Feel free to visit Issues or Discussions!

---