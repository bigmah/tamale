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

    /// The viewer gives the whole screen to the page, and its controls get out of the way.
    func testTheViewerFillsTheScreenAndItsControlsHideWhileReading() throws {
        let env = ProcessInfo.processInfo.environment
        guard let key = env["TAMALE_DEBUG_KEY"], let hub = env["TAMALE_DEBUG_HUB"] else {
            throw XCTSkip("run apple/scripts/uitest.sh")
        }
        let app = XCUIApplication()
        app.launchEnvironment = ["TAMALE_DEBUG_KEY": key, "TAMALE_DEBUG_HUB": hub]
        app.launch()
        let row = text(app, containing: "Show me the plan")
        wait(row, app, timeout: 20)
        row.tap()
        let yes = app.buttons["Yes"]
        wait(yes, app, timeout: 20)
        yes.tap()

        // Inline: the card runs edge to edge, and a short page gets a short card.
        let inline = app.webViews.firstMatch
        wait(inline, app, timeout: 15)
        wait(inline.staticTexts["Plan"], app, timeout: 15)
        XCTAssertEqual(inline.frame.width, app.frame.width, accuracy: 1)
        let short = NSPredicate { _, _ in inline.frame.height < 150 }
        wait(for: [expectation(for: short, evaluatedWith: nil)], timeout: 10)

        let field = app.textFields.firstMatch.exists ? app.textFields.firstMatch : app.textViews.firstMatch
        field.tap()
        field.typeText("long")
        app.buttons["Send"].tap()
        // An approval takes the keyboard down, leaving room to read what it's about.
        wait(yes, app, timeout: 15)
        XCTAssertFalse(app.keyboards.firstMatch.waitForExistence(timeout: 1))
        yes.tap()

        let card = app.buttons["Launch checklist"]
        wait(card, app, timeout: 15)
        let loaded = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: card)
        wait(for: [loaded], timeout: 15)
        card.tap()

        let done = app.buttons["Done"]
        wait(done, app, timeout: 10)
        XCTAssertEqual(app.webViews.firstMatch.frame, app.frame)

        // Reading hides the controls, leaving a handle that brings them back.
        app.webViews.firstMatch.swipeUp()
        let handle = app.buttons["Show controls"]
        wait(handle, app, timeout: 5)
        XCTAssertFalse(done.exists)
        handle.tap()
        wait(done, app, timeout: 5)

        // Scrolling back up does too.
        app.webViews.firstMatch.swipeUp()
        wait(handle, app, timeout: 5)
        app.webViews.firstMatch.swipeDown()
        wait(done, app, timeout: 5)

        // The end of the page clears the bar.
        for _ in 0..<12 { app.webViews.firstMatch.swipeUp(velocity: .fast) }
        // (The conversation's card, underneath, has the same page.)
        let footer = app.webViews.firstMatch.staticTexts["The end of the page"]
        wait(footer, app, timeout: 5)
        wait(done, app, timeout: 5)
        XCTAssertLessThan(footer.frame.maxY, done.frame.minY)

        // The title goes back to the top, and from there a pull down closes it.
        // (The conversation's card, underneath, has the same name.)
        app.buttons["artifact-title"].tap()
        wait(app.webViews.firstMatch.staticTexts["Step 1"], app, timeout: 5)
        Thread.sleep(forTimeInterval: 1)
        let web = app.webViews.firstMatch
        web.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.3))
            .press(forDuration: 0.05, thenDragTo: web.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.95)))
        let closed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: done)
        wait(for: [closed], timeout: 5)
        wait(card, app, timeout: 5)
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
        XCTAssertEqual(app.buttons["artifact-title"].label, "Echo")
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
