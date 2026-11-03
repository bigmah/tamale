import XCTest

/// Drives the app against a local tamale-hub and the stand-in session from src/web.rs. Run by
/// apple/scripts/uitest.sh, which passes TAMALE_DEBUG_KEY and TAMALE_DEBUG_HUB.
final class TamaleUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Waits for `element`, printing the screen's elements if it doesn't come.
    func wait(_ element: XCUIElement, _ app: XCUIApplication, timeout: Double = 15, line: UInt = #line) {
        if element.waitForExistence(timeout: timeout) { return }
        print("SCREEN AT LINE \(line):\n\(app.debugDescription)")
        XCTFail("waited for \(element)", line: line)
    }

    func text(_ app: XCUIApplication, containing: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", containing)).firstMatch
    }

    /// The code /remote shows is a session link, without the key: scanned (here, pasted) on a
    /// fresh install, the app asks for the key, then opens the session. Starts
    /// from no keys at all.
    func testASessionCodeBeforeTheKeyOpensTheSessionOnceTheKeyIsAdded() throws {
        let env = ProcessInfo.processInfo.environment
        guard let key = env["TAMALE_DEBUG_KEY"], let hub = env["TAMALE_DEBUG_HUB"], let sid = env["TAMALE_DEBUG_SID"] else {
            throw XCTSkip("run apple/scripts/uitest.sh")
        }
        let app = XCUIApplication()
        app.launchEnvironment = ["TAMALE_DEBUG_RESET": "1"]
        app.launch()
        let add = app.buttons["Add a key"]
        wait(add, app, timeout: 10)
        add.tap()
        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        wait(field, app, timeout: 5)
        field.tap()
        field.typeText("\(hub)/open/\(sid)")
        wait(text(app, containing: "That's a session's code"), app, timeout: 5)
        let keyField = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        keyField.tap()
        keyField.typeText(key)
        app.navigationBars["Add a key"].buttons["Add"].tap()
        // Straight into the session, once the sheet has gone.
        wait(text(app, containing: "Here it is."), app, timeout: 20)
    }

    func testAddsAKeyOpensASessionAndDrivesIt() throws {
        let env = ProcessInfo.processInfo.environment
        guard let key = env["TAMALE_DEBUG_KEY"], let hub = env["TAMALE_DEBUG_HUB"] else {
            throw XCTSkip("run apple/scripts/uitest.sh")
        }
        let app = XCUIApplication()
        app.launchEnvironment = ["TAMALE_DEBUG_KEY": key, "TAMALE_DEBUG_HUB": hub]
        app.launch()

        // The list: the stand-in session, its sealed title opened with the key.
        let row = text(app, containing: "Show me the plan")
        wait(row, app, timeout: 20)
        XCTAssertTrue(text(app, containing: "fake-host:~/dev/fake").exists)
        row.tap()

        // The conversation, over the encrypted channel.
        wait(text(app, containing: "Here it is."), app, timeout: 20)
        wait(text(app, containing: "The plan"), app, timeout: 10)

        // The approval.
        let yes = app.buttons["Yes"]
        wait(yes, app, timeout: 10)
        XCTAssertTrue(text(app, containing: "Allow bash?").exists)
        yes.tap()
        wait(text(app, containing: "approved: Yes"), app, timeout: 10)

        // A prompt, and its reply with an artifact.
        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        wait(field, app, timeout: 5)
        field.tap()
        field.typeText("hello from the phone")
        app.buttons["Send"].tap()
        wait(text(app, containing: "You said hello from the phone"), app, timeout: 15)

        // The artifact, full screen, rendered by WebKit.
        let echo = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Echo")).firstMatch
        wait(echo, app, timeout: 15)
        let inline = app.webViews.staticTexts["hello from the phone"]
        wait(inline, app, timeout: 15)
        // Enabled once the page has arrived.
        let loaded = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: echo)
        wait(for: [loaded], timeout: 15)
        echo.tap()
        wait(app.buttons["Done"], app, timeout: 10)
        XCTAssertTrue(app.navigationBars["Echo"].exists)
        app.buttons["Done"].tap()

        app.navigationBars.buttons.element(boundBy: 0).tap()
        // The library keeps what was opened.
        app.buttons["Library"].tap()
        wait(text(app, containing: "Echo"), app, timeout: 10)
        XCTAssertTrue(text(app, containing: "The plan").exists)

        // After a relaunch, without the test key: the key came back from the Keychain and
        // the library from the store.
        app.terminate()
        app.launchEnvironment = [:]
        app.launch()
        wait(text(app, containing: "Show me the plan"), app, timeout: 20)
        app.buttons["Library"].tap()
        wait(text(app, containing: "Echo"), app, timeout: 10)
        text(app, containing: "Echo").tap()
        wait(app.webViews.staticTexts["hello from the phone"], app, timeout: 10)
        app.buttons["Done"].tap()
    }
}
